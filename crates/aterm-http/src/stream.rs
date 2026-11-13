// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deadline-bounded TCP/TLS byte stream with revocable [`Guard`] authority checked per I/O step.

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(any(windows, test))]
mod native_dns;

/// Use the broker-aware resolver on Unix and bounded system resolution on Windows.
fn resolve_host(host: &str, within: Duration) -> io::Result<Vec<IpAddr>> {
    #[cfg(unix)]
    {
        aterm_uds::lan::resolve(host, within).map_err(aterm_uds::lan::LanError::into_io)
    }
    #[cfg(windows)]
    {
        native_dns::resolve(host, within)
    }
}

/// Revocable permission checked before every I/O step.
pub trait Guard: Send + Sync + std::fmt::Debug {
    /// Returns `true` if the request may still proceed.
    fn is_authorized(&self) -> bool;
}

/// [`Guard`] implementation that always permits requests.
#[derive(Clone, Copy, Debug)]
pub struct AlwaysAuthorized;

impl Guard for AlwaysAuthorized {
    #[inline]
    fn is_authorized(&self) -> bool {
        true
    }
}

/// Returns the [`io::ErrorKind::PermissionDenied`] error for a revoked guard.
#[must_use]
pub fn revoked_error() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "request authority revoked")
}

/// Wall-clock deadline for a request with an optional per-call idle timeout cap.
#[derive(Clone, Copy, Debug)]
pub struct Deadline {
    at: Instant,
    idle: Option<Duration>,
    budget: Budget,
}

/// Which budget a [`Deadline`]'s total is, for the text its expiry carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Budget {
    /// The whole request's (the client timeout, or a backstop).
    Request,
    /// The connect phase's: DNS, connect, proxy CONNECT and TLS handshake.
    Connect,
}

/// Bound from which a socket call's timeout was derived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cap {
    /// Remaining total request deadline.
    Total,
    /// Per-call idle timeout cap.
    Idle(Duration),
}

/// The end a budget too long for an [`Instant`] gets instead: `u32::MAX`
/// seconds, about 136 years, which every platform's clock can hold.
const FAR_FUTURE: Duration = Duration::from_secs(4_294_967_295);

/// `now + budget`, or `now + FAR_FUTURE` when that sum overflows the clock.
fn later(now: Instant, budget: Duration) -> Instant {
    now.checked_add(budget)
        .or_else(|| now.checked_add(FAR_FUTURE))
        .unwrap_or(now)
}

impl Deadline {
    /// Keep an enclosing operation's original absolute bound without restarting its clock.
    #[must_use]
    pub(crate) fn until(mut self, until: Instant) -> Self {
        if until < self.at {
            self.at = until;
        }
        self
    }
    /// Creates a deadline `budget` from now with no idle cap.
    #[must_use]
    pub fn after(budget: Duration) -> Self {
        Self {
            at: later(Instant::now(), budget),
            idle: None,
            budget: Budget::Request,
        }
    }

    /// A deadline with no caller-visible whole-operation wall bound. Socket operations still
    /// take their idle cap; the far-future instant is only the finite representation required
    /// by platform timeout APIs.
    #[must_use]
    pub(crate) fn unbounded() -> Self {
        Self {
            at: later(Instant::now(), FAR_FUTURE),
            idle: None,
            budget: Budget::Request,
        }
    }

    /// Caps each individual socket call at `idle`.
    #[inline]
    #[must_use]
    pub const fn with_idle(mut self, idle: Duration) -> Self {
        self.idle = Some(idle);
        self
    }

    /// Returns the per-call idle cap, if set.
    #[inline]
    #[must_use]
    pub const fn idle(&self) -> Option<Duration> {
        self.idle
    }

    /// Narrows the deadline to at most `within` from now for the connect phase.
    #[must_use]
    pub(crate) fn connect_phase(self, within: Duration) -> Self {
        let at = later(Instant::now(), within);
        if at < self.at {
            Self {
                at,
                idle: self.idle,
                budget: Budget::Connect,
            }
        } else {
            self
        }
    }

    /// Returns remaining time before the deadline expires, or `None` if expired.
    #[must_use]
    pub fn remaining(&self) -> Option<Duration> {
        self.at
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
    }

    /// Returns the remaining budget or a timeout error if expired.
    /// # Errors
    /// Returns [`io::ErrorKind::TimedOut`] once the deadline has passed.
    pub fn remaining_or_timeout(&self) -> io::Result<Duration> {
        self.remaining().ok_or_else(|| self.expired(Cap::Total))
    }

    /// Returns `min(remaining, idle)` and which [`Cap`] bound applies for the next socket call.
    pub(crate) fn per_call(&self) -> io::Result<(Duration, Cap)> {
        let remaining = self.remaining_or_timeout()?;
        match self.idle {
            Some(idle) if idle.is_zero() => Err(self.expired(Cap::Idle(idle))),
            Some(idle) if idle < remaining => Ok((idle, Cap::Idle(idle))),
            _ => Ok((remaining, Cap::Total)),
        }
    }

    /// Constructs the timeout error for the expired `cap` bound.
    pub(crate) fn expired(&self, cap: Cap) -> io::Error {
        let text = match (cap, self.budget) {
            (Cap::Idle(idle), _) => format!("no bytes for {}ms", idle.as_millis()),
            (Cap::Total, Budget::Request) => "request deadline exceeded".to_owned(),
            (Cap::Total, Budget::Connect) => "connect deadline exceeded".to_owned(),
        };
        io::Error::new(io::ErrorKind::TimedOut, text)
    }

    /// Normalizes `TimedOut`/`WouldBlock` socket errors into [`Self::expired`].
    pub(crate) fn as_timeout(&self, error: io::Error, cap: Cap) -> io::Error {
        match error.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => self.expired(cap),
            _ => error,
        }
    }
}

/// Opens the underlying TCP connection for a request.
pub trait Connect: Send + Sync + std::fmt::Debug {
    /// Connects to `host:port` within `deadline`.
    /// # Errors
    /// Returns any resolution, connection, or peer-verification error.
    fn connect(&self, host: &str, port: u16, deadline: Deadline) -> io::Result<TcpStream>;
}

/// Default [`Connect`] implementation using bounded system DNS resolution.
#[derive(Clone, Copy, Debug, Default)]
pub struct TcpConnector;

impl Connect for TcpConnector {
    fn connect(&self, host: &str, port: u16, deadline: Deadline) -> io::Result<TcpStream> {
        self.connect_noting(host, port, deadline, &mut |_, _| {})
    }
}

impl TcpConnector {
    /// [`Connect::connect`], telling `failed` the address of each connect that failed and
    /// why, before the next address is tried: the error alone does not name the address it
    /// came from, which a caller that judges a refusal by where it went (a LAN address, a
    /// public one) needs. A connect's error returned is the last one `failed` was told; a
    /// failed lookup, or a deadline spent before the next address, is told nothing. Each
    /// address the lookup gave gets an even share of what the deadline has left, the last one
    /// all of it, so one that never answers leaves the next its turn.
    ///
    /// # Errors
    ///
    /// As [`Connect::connect`].
    pub fn connect_noting(
        &self,
        host: &str,
        port: u16,
        deadline: Deadline,
        failed: &mut dyn FnMut(SocketAddr, &io::Error),
    ) -> io::Result<TcpStream> {
        connect_with(host, port, deadline, resolve_host, failed)
    }
}

/// [`TcpConnector::connect_noting`] over `resolver` ([`resolve_host`], which on macOS has the root
/// LAN broker resolve a local name), within what the deadline has left. Each address is dialled by
/// [`crate::connect::connect_timeout`], which on macOS has the broker dial a local-network one.
fn connect_with(
    host: &str,
    port: u16,
    deadline: Deadline,
    resolver: impl FnOnce(&str, Duration) -> io::Result<Vec<IpAddr>>,
    failed: &mut dyn FnMut(SocketAddr, &io::Error),
) -> io::Result<TcpStream> {
    let within = deadline.remaining_or_timeout()?;
    let addrs = lookup(host, port, within, resolver)?;
    connect_each(
        host,
        &addrs,
        &deadline,
        crate::connect::connect_timeout,
        failed,
    )
}

/// `host`'s addresses on `port`, by `resolver` within `within`, a failure named as the
/// resolution's (`dns: <host>: …`) with its kind kept: on macOS a missing LAN broker for a local
/// name is `NotConnected`, its words naming what installs it.
fn lookup(
    host: &str,
    port: u16,
    within: Duration,
    resolver: impl FnOnce(&str, Duration) -> io::Result<Vec<IpAddr>>,
) -> io::Result<Vec<SocketAddr>> {
    match resolver(host, within) {
        Ok(found) => Ok(found
            .into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect()),
        Err(error) => {
            let error = io::Error::new(error.kind(), format!("dns: {host}: {error}"));
            // Only the resolver's own answer can lack mDNS.
            #[cfg(target_os = "linux")]
            let error = without_mdns(host, error);
            Err(error)
        }
    }
}

/// Connect by `connect` to the first of `addrs`, `host`'s, that answers within `deadline`,
/// telling `failed` of each that did not ([`TcpConnector::connect_noting`]). Each address still
/// to try gets an even share of what is left, the last one all of it: an address that never
/// answers (a stale mDNS answer for a Mac that moved, a broken IPv6 path) leaves the ones after it
/// their turn, and the time a prompt refusal leaves goes to them.
fn connect_each(
    host: &str,
    addrs: &[SocketAddr],
    deadline: &Deadline,
    mut connect: impl FnMut(&SocketAddr, Duration) -> io::Result<TcpStream>,
    failed: &mut dyn FnMut(SocketAddr, &io::Error),
) -> io::Result<TcpStream> {
    let mut last = None;
    for (tried, addr) in addrs.iter().enumerate() {
        let (budget, cap) = deadline.per_call()?;
        let left = u32::try_from(addrs.len() - tried).unwrap_or(u32::MAX);
        // Never a zero timeout, which a connect refuses as invalid rather than spends.
        let share = (budget / left).max(Duration::from_millis(1)).min(budget);
        match connect(addr, share) {
            Ok(stream) => {
                stream.set_nodelay(true)?;
                return Ok(stream);
            }
            Err(error) => {
                let timed_out = matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                );
                let error = if timed_out && share < budget {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "no answer within its share of the deadline ({}ms)",
                            share.as_millis()
                        ),
                    )
                } else {
                    deadline.as_timeout(error, cap)
                };
                failed(*addr, &error);
                last = Some(error);
            }
        }
    }
    Err(last.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("dns: {host}: resolved to no addresses"),
        )
    }))
}

/// `error`, a failed lookup of `host`, with what Linux needs to resolve a local
/// name when `host` is one: glibc asks mDNS only through Avahi's nss-mdns (or
/// systemd-resolved with MulticastDNS on), which many installs lack, and then
/// says only that the name is unknown. A lookup that timed out is left as it
/// is, and so is its kind, which callers judge.
#[cfg(all(unix, any(test, target_os = "linux")))]
fn without_mdns(host: &str, error: io::Error) -> io::Error {
    let local = host
        .strip_suffix('.')
        .unwrap_or(host)
        .rsplit_once('.')
        .is_some_and(|(name, label)| !name.is_empty() && label.eq_ignore_ascii_case("local"));
    if error.kind() == io::ErrorKind::TimedOut || !local {
        return error;
    }
    io::Error::new(error.kind(), format!("{error} ({NO_MDNS})"))
}

/// What a failed lookup of a `.local` name adds on Linux (`dns: <host>: … (…)`), where glibc
/// resolves one over mDNS only through a resolver many installs lack.
#[cfg(all(unix, any(test, target_os = "linux")))]
const NO_MDNS: &str = "this system resolves .local names over mDNS only through Avahi's \
     nss-mdns or systemd-resolved with MulticastDNS on; a URL naming the server's address needs \
     neither";

/// A socket carrying the request's deadline and authority, re-checked before
/// every call made on it.
struct Bounded {
    sock: TcpStream,
    guard: Arc<dyn Guard>,
    deadline: Deadline,
}

impl Bounded {
    /// The request may still proceed: authority holds and budget remains.
    fn admit(&self) -> io::Result<(Duration, Cap)> {
        if !self.guard.is_authorized() {
            return Err(revoked_error());
        }
        self.deadline.per_call()
    }

    /// [`Self::admit`], then that timeout on the one socket direction the
    /// coming call uses.
    fn admit_read(&self) -> io::Result<Cap> {
        let (budget, cap) = self.admit()?;
        self.timeout_set(self.sock.set_read_timeout(Some(budget)))?;
        Ok(cap)
    }

    /// As [`Self::admit_read`], for a write.
    fn admit_write(&self) -> io::Result<Cap> {
        let (budget, cap) = self.admit()?;
        self.timeout_set(self.sock.set_write_timeout(Some(budget)))?;
        Ok(cap)
    }

    /// Whether the coming call may go ahead after setting its timeout.
    fn timeout_set(&self, set: io::Result<()>) -> io::Result<()> {
        match set {
            Ok(()) => Ok(()),
            Err(error)
                if cfg!(target_vendor = "apple") && error.kind() == io::ErrorKind::InvalidInput =>
            {
                Ok(())
            }
            Err(error) => Err(match self.sock.take_error() {
                Ok(Some(pending)) => pending,
                _ => error,
            }),
        }
    }
}

impl Read for Bounded {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let cap = self.admit_read()?;
        self.sock
            .read(buf)
            .map_err(|error| self.deadline.as_timeout(error, cap))
    }
}

impl Write for Bounded {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let cap = self.admit_write()?;
        self.sock
            .write(buf)
            .map_err(|error| self.deadline.as_timeout(error, cap))
    }

    fn flush(&mut self) -> io::Result<()> {
        let cap = self.admit_write()?;
        self.sock
            .flush()
            .map_err(|error| self.deadline.as_timeout(error, cap))
    }
}

/// Plaintext or TLS TCP stream enforcing a [`Deadline`] and [`Guard`].
pub struct Stream {
    inner: Inner,
    /// A write failed on the socket. From then on a TLS read takes only what
    /// the peer sent: rustls's own `read` first retries the write it still
    /// holds, fails on it again, and never reaches an answer the peer sent
    /// before it closed (a 413 over a body it would not read).
    write_failed: bool,
}

enum Inner {
    Plain(Bounded),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, Bounded>>),
}

/// A cancellation handle which can close a stream's socket but cannot read or write it.
#[derive(Debug)]
pub struct Interrupt {
    socket: TcpStream,
}

impl Interrupt {
    /// Wake pending I/O and prevent further network operations on the stream.
    pub fn interrupt(&self) {
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
    }
}

impl Stream {
    /// A separate handle that wakes blocked I/O when a background operation is cancelled.
    pub fn interruptor(&self) -> io::Result<Interrupt> {
        Ok(Interrupt {
            socket: self.socket().sock.try_clone()?,
        })
    }

    /// Wraps an established plaintext [`TcpStream`].
    #[must_use]
    pub fn plain(tcp: TcpStream, guard: Arc<dyn Guard>, deadline: Deadline) -> Self {
        Self {
            inner: Inner::Plain(Bounded {
                sock: tcp,
                guard,
                deadline,
            }),
            write_failed: false,
        }
    }

    /// Completes a TLS handshake over an established connection.
    /// # Errors
    /// Returns an error if `server_name` is invalid, the handshake fails, or the guard/deadline trips.
    pub fn start_tls(
        tcp: TcpStream,
        config: Arc<rustls::ClientConfig>,
        server_name: &str,
        guard: Arc<dyn Guard>,
        deadline: Deadline,
    ) -> io::Result<Self> {
        let name = rustls::pki_types::ServerName::try_from(server_name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid TLS server name"))?
            .to_owned();
        let connection = rustls::ClientConnection::new(config, name)
            .map_err(|error| io::Error::other(format!("TLS setup failed: {error}")))?;
        let mut tls = Box::new(rustls::StreamOwned::new(
            connection,
            Bounded {
                sock: tcp,
                guard,
                deadline,
            },
        ));
        let rustls::StreamOwned { conn, sock } = &mut *tls;
        while conn.is_handshaking() {
            let (read, wrote) = conn.complete_io(sock).map_err(|error| {
                // Preserve revocation and timeout error kinds.
                match error.kind() {
                    io::ErrorKind::PermissionDenied | io::ErrorKind::TimedOut => error,
                    kind => io::Error::new(kind, format!("TLS handshake failed: {error}")),
                }
            })?;
            if read == 0 && wrote == 0 && conn.is_handshaking() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "peer closed during TLS handshake",
                ));
            }
        }
        Ok(Self {
            inner: Inner::Tls(tls),
            write_failed: false,
        })
    }

    /// Extracts the inner [`TcpStream`] from a plaintext stream, or `None` if already under TLS.
    #[must_use]
    pub fn into_tcp(self) -> Option<TcpStream> {
        match self.inner {
            Inner::Plain(bounded) => Some(bounded.sock),
            Inner::Tls(_) => None,
        }
    }

    /// Replaces the active [`Deadline`] on this stream.
    pub fn set_deadline(&mut self, deadline: Deadline) {
        match &mut self.inner {
            Inner::Plain(bounded) => bounded.deadline = deadline,
            Inner::Tls(tls) => tls.sock.deadline = deadline,
        }
    }

    /// The socket, whichever arm holds it.
    fn socket(&self) -> &Bounded {
        match &self.inner {
            Inner::Plain(bounded) => bounded,
            Inner::Tls(tls) => &tls.sock,
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.socket().admit()?;
        match &mut self.inner {
            Inner::Plain(bounded) => bounded.read(buf),
            Inner::Tls(tls) if self.write_failed => read_received(tls, buf),
            Inner::Tls(tls) => tls.read(buf),
        }
    }
}

/// Plaintext the TLS peer sent, read without first finishing a write rustls
/// holds back ([`Stream::write_failed`]): records off the socket, through
/// [`Bounded`] as every other socket call, until plaintext, the peer's
/// close, or an error.
fn read_received(
    tls: &mut rustls::StreamOwned<rustls::ClientConnection, Bounded>,
    buf: &mut [u8],
) -> io::Result<usize> {
    let rustls::StreamOwned { conn, sock } = tls;
    loop {
        match conn.reader().read(buf) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            done => return done,
        }
        match conn.read_tls(sock) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
        conn.process_new_packets()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    }
}

impl Stream {
    /// Note a socket write's failure (see [`Self::write_failed`]). A signal's
    /// interruption is no failure: the write is retried.
    fn wrote<T>(&mut self, wrote: io::Result<T>) -> io::Result<T> {
        if let Err(error) = &wrote
            && error.kind() != io::ErrorKind::Interrupted
        {
            self.write_failed = true;
        }
        wrote
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.socket().admit()?;
        let wrote = match &mut self.inner {
            Inner::Plain(bounded) => bounded.write(buf),
            Inner::Tls(tls) => tls.write(buf),
        };
        self.wrote(wrote)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.socket().admit()?;
        let flushed = match &mut self.inner {
            Inner::Plain(bounded) => bounded.flush(),
            Inner::Tls(tls) => tls.flush(),
        };
        self.wrote(flushed)
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
