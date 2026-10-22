// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The byte stream under the HTTP layer: TCP, optionally wrapped in TLS, with
//! a global deadline and a revocable authority checked on every I/O step.
//!
//! # The authority guard
//!
//! A caller's permission to speak can be revoked from another thread WHILE a
//! request is in flight (whatever asked for the request goes away, or the
//! caller's authority epoch moves). DNS, `connect`, proxy negotiation and the
//! TLS handshake can all block for seconds before a single body byte moves, so
//! checking authority once at the top would be almost meaningless.
//!
//! [`Guard`] is therefore re-checked at every read and every write, which are
//! the points at which request data could actually leave this process or a
//! response could be admitted. The check is two atomic loads, so revocation on
//! the revoking thread stays wait-free.
//!
//! # Deadlines
//!
//! One deadline covers the whole request. It is converted to a per-syscall
//! socket timeout before EVERY socket call, so a peer that trickles bytes cannot extend the total.
//! An idle cap ([`Deadline::with_idle`]) bounds each of those calls further, at
//! `min(remaining, idle)`, so a peer that goes quiet is caught without waiting
//! for the total; the expiry names the bound that fired. Name resolution is
//! bounded too: std's resolver takes no timeout, so [`TcpConnector`] resolves
//! on a helper thread it waits for at most the remaining budget.
//!
//! "Every socket call" includes the ones this module does not issue itself.
//! rustls drives the socket in loops: the handshake's `complete_io` stays in
//! `read_tls` until a flight is whole, and an established stream's `read`
//! loops `complete_io` until a whole RECORD has arrived (16 KiB, which a peer
//! can send a byte at a time). A timeout applied once before either loop bounds
//! one of its reads while every dribbled byte restarts the clock, and the guard
//! is never asked again until the loop ends. So the socket rustls holds is
//! [`Bounded`], which re-checks the guard and re-derives the remaining budget
//! before each call it makes — for the stream's whole life, not only its
//! handshake.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// A revocable permission to keep speaking, re-checked at every I/O step.
pub trait Guard: Send + Sync + std::fmt::Debug {
    /// Whether the request may still proceed. Must be cheap and wait-free.
    fn is_authorized(&self) -> bool;
}

/// A [`Guard`] that always permits — for callers with no revocation model.
#[derive(Clone, Copy, Debug)]
pub struct AlwaysAuthorized;

impl Guard for AlwaysAuthorized {
    fn is_authorized(&self) -> bool {
        true
    }
}

/// The error a revoked authority produces. Callers match on
/// [`io::ErrorKind::PermissionDenied`] to distinguish it from a network fault.
#[must_use]
pub fn revoked_error() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "request authority revoked")
}

/// A single wall-clock budget for an entire request, and optionally an idle
/// cap on each socket call made under it.
///
/// The total is what [`Self::remaining`] counts down. The idle cap
/// ([`Self::with_idle`]) bounds every single wait, so a peer that goes quiet
/// is caught long before the total runs out: each read, write and connect
/// waits at most `min(remaining, idle)`. An expiry names which of the two
/// fired: `request deadline exceeded` (or `connect deadline exceeded` for the
/// budget [`crate::RequestBuilder::connect_within`] sets), or
/// `no bytes for {ms}ms` for the idle cap, in whole milliseconds.
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

/// Which bound one socket call's timeout came from, so its expiry is reported
/// as the bound that actually fired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cap {
    /// The remaining total.
    Total,
    /// The idle cap, shorter than the remaining total.
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
    /// A deadline `budget` from now, with no idle cap.
    ///
    /// A budget past what an [`Instant`] can hold (`Duration::MAX`, meaning
    /// "no bound") ends about 136 years from now instead of panicking.
    #[must_use]
    pub fn after(budget: Duration) -> Self {
        Self {
            at: later(Instant::now(), budget),
            idle: None,
            budget: Budget::Request,
        }
    }

    /// This deadline with every socket call also capped at `idle`.
    #[must_use]
    pub const fn with_idle(mut self, idle: Duration) -> Self {
        self.idle = Some(idle);
        self
    }

    /// The idle cap, if one is set.
    #[must_use]
    pub const fn idle(&self) -> Option<Duration> {
        self.idle
    }

    /// The connect phase's deadline: `within` from now, or this deadline's
    /// own end if that comes first, with the same idle cap.
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

    /// Time left, or `None` once the budget is spent.
    #[must_use]
    pub fn remaining(&self) -> Option<Duration> {
        self.at
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
    }

    /// The remaining budget, or a timeout error if it is gone.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::TimedOut`] once the deadline has passed.
    pub fn remaining_or_timeout(&self) -> io::Result<Duration> {
        self.remaining().ok_or_else(|| self.expired(Cap::Total))
    }

    /// The timeout for one socket call, `min(remaining, idle)`, and which
    /// bound it is.
    pub(crate) fn per_call(&self) -> io::Result<(Duration, Cap)> {
        let remaining = self.remaining_or_timeout()?;
        match self.idle {
            Some(idle) if idle.is_zero() => Err(self.expired(Cap::Idle(idle))),
            Some(idle) if idle < remaining => Ok((idle, Cap::Idle(idle))),
            _ => Ok((remaining, Cap::Total)),
        }
    }

    /// The expiry error of the bound `cap` names.
    pub(crate) fn expired(&self, cap: Cap) -> io::Error {
        let text = match (cap, self.budget) {
            (Cap::Idle(idle), _) => format!("no bytes for {}ms", idle.as_millis()),
            (Cap::Total, Budget::Request) => "request deadline exceeded".to_owned(),
            (Cap::Total, Budget::Connect) => "connect deadline exceeded".to_owned(),
        };
        io::Error::new(io::ErrorKind::TimedOut, text)
    }

    /// Normalise a socket-timeout error into the expiry of the bound that
    /// fired.
    ///
    /// A blocking socket that hits `SO_RCVTIMEO` reports `WouldBlock` on Unix
    /// and `TimedOut` on Windows. The only timeout this socket ever carries is
    /// the one [`Self::per_call`] chose, so either kind means that bound
    /// expired and callers should not have to know the platform to see it.
    /// (It also matters inside `complete_io`, which treats `WouldBlock` as
    /// "come back later" rather than as a failure.)
    pub(crate) fn as_timeout(&self, error: io::Error, cap: Cap) -> io::Error {
        match error.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => self.expired(cap),
            _ => error,
        }
    }
}

/// Opens the TCP connection a request will run over.
///
/// This is the seam a caller replaces when it must connect to a socket address
/// pinned in advance and attest the peer process on the ESTABLISHED four-tuple
/// before any request byte is written, which a hostname-based connect could
/// not do (the retired managed-Ollama path did exactly that).
pub trait Connect: Send + Sync + std::fmt::Debug {
    /// Connect to `host:port`, respecting `deadline`.
    ///
    /// # Errors
    ///
    /// Any resolution, connection, or (for an attesting connector) peer
    /// verification failure.
    fn connect(&self, host: &str, port: u16, deadline: Deadline) -> io::Result<TcpStream>;
}

/// Resolve-and-connect over the system resolver — the default.
///
/// Resolution is bounded: std's resolver takes no timeout, so it runs on a
/// helper thread the request waits for at most the remaining budget. Every
/// resolution failure's text starts `dns:`, the same on every platform.
#[derive(Clone, Copy, Debug, Default)]
pub struct TcpConnector;

impl Connect for TcpConnector {
    fn connect(&self, host: &str, port: u16, deadline: Deadline) -> io::Result<TcpStream> {
        use std::net::ToSocketAddrs;
        // An expired request must not start a lookup.
        let within = deadline.remaining_or_timeout()?;
        let addrs = resolve_within(host, port, within, |host, port| {
            (host, port).to_socket_addrs()
        })?;
        let mut last = None;
        for addr in addrs {
            // Re-derive per address, or the global timeout is a per-address one.
            let (budget, cap) = deadline.per_call()?;
            // The crate's own connect: a refusal is prompt on Windows too, so
            // a short per-call budget never reads a refusal as a timeout.
            match crate::connect::connect_timeout(&addr, budget) {
                Ok(stream) => {
                    stream.set_nodelay(true)?;
                    return Ok(stream);
                }
                Err(error) => last = Some(deadline.as_timeout(error, cap)),
            }
        }
        Err(last.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("dns: {host}: resolved to no addresses"),
            )
        }))
    }
}

/// Resolve `host:port` through `resolver` on a helper thread, waiting at most
/// `within` for its answer.
///
/// Past `within` the error is `TimedOut` with `dns: no answer within {ms}ms`,
/// and the helper is left to finish alone: its one send fails quietly once
/// this receiver is gone. A lookup error keeps its kind, its text prefixed
/// `dns: {host}: `. When no helper can be spawned the request fails without
/// running the potentially unbounded resolver on the request's thread.
fn resolve_within<F, I>(
    host: &str,
    port: u16,
    within: Duration,
    resolver: F,
) -> io::Result<Vec<SocketAddr>>
where
    F: FnOnce(&str, u16) -> io::Result<I> + Send + 'static,
    I: Iterator<Item = SocketAddr>,
{
    resolve_within_using(host, port, within, resolver, |lookup| {
        std::thread::Builder::new()
            .name("aterm-http-dns".into())
            .spawn(lookup)
            .map(drop)
    })
}

/// The resolver path with helper creation injected, so a refused spawn can be
/// checked without exhausting the process's threads or calling the system resolver.
fn resolve_within_using<F, I>(
    host: &str,
    port: u16,
    within: Duration,
    resolver: F,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<()>,
) -> io::Result<Vec<SocketAddr>>
where
    F: FnOnce(&str, u16) -> io::Result<I> + Send + 'static,
    I: Iterator<Item = SocketAddr>,
{
    /// One lookup, its failure named as a resolution failure.
    fn lookup<F, I>(resolver: F, host: &str, port: u16) -> io::Result<Vec<SocketAddr>>
    where
        F: FnOnce(&str, u16) -> io::Result<I>,
        I: Iterator<Item = SocketAddr>,
    {
        resolver(host, port)
            .map(Iterator::collect)
            .map_err(|error| io::Error::new(error.kind(), format!("dns: {host}: {error}")))
    }
    let (tx, rx) = mpsc::sync_channel(1);
    let helper_host = host.to_owned();
    spawn(Box::new(move || {
        // The receiver may be gone (the request timed out): the answer is
        // dropped, which is all a late lookup can do. A failed spawn drops
        // this closure without invoking the resolver.
        let _ = tx.send(lookup(resolver, &helper_host, port));
    }))
    .map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("dns: {host}: could not start the resolver: {error}"),
        )
    })?;
    match rx.recv_timeout(within) {
        Ok(answer) => answer,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("dns: no answer within {}ms", within.as_millis()),
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::other(format!(
            "dns: {host}: the resolver ended without an answer"
        ))),
    }
}

/// A socket carrying the request's deadline and authority, re-checked before
/// every call made on it. Both arms of [`Stream`] own their socket through
/// this; under TLS it is the socket rustls itself drives (see the module docs).
struct Bounded {
    sock: TcpStream,
    guard: Arc<dyn Guard>,
    deadline: Deadline,
}

impl Bounded {
    /// The request may still proceed: authority holds and budget remains.
    /// Answers the coming call's timeout, `min(remaining, idle)`.
    fn admit(&self) -> io::Result<(Duration, Cap)> {
        if !self.guard.is_authorized() {
            return Err(revoked_error());
        }
        self.deadline.per_call()
    }

    /// [`Self::admit`], then that timeout on the one socket direction the
    /// coming call uses. The budget only shrinks, so no call outlives the
    /// deadline by more than itself.
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
    ///
    /// On Apple platforms every `setsockopt` fails with EINVAL once the socket
    /// is shut down both ways, as a peer's reset leaves it, yet the bytes the
    /// peer sent before the reset stay readable (an early 401 or 429). Such a
    /// socket cannot block (a read returns those bytes, EOF or the reset at
    /// once; a write fails at once), so the call goes ahead and reports what
    /// it finds, and the answer is not lost to the setter. EINVAL means that
    /// shut socket, since [`Deadline::per_call`] never yields a zero or
    /// out-of-range timeout. Any other setter failure is reported as the
    /// socket's pending error, else as itself, with no socket call.
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

/// A TCP stream, optionally under TLS, carrying the deadline and the guard.
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

impl Stream {
    /// Wrap an established plaintext connection.
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

    /// Complete a TLS handshake over an established connection.
    ///
    /// `server_name` is the origin host even through a proxy tunnel, so a proxy
    /// cannot substitute its own certificate.
    ///
    /// # Errors
    ///
    /// An invalid server name, or any handshake or certificate failure.
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
        // Drive the handshake now so a certificate failure surfaces here, not as
        // a confusing short write later. `Bounded` holds deadline and guard for
        // every syscall the handshake makes.
        let rustls::StreamOwned { conn, sock } = &mut *tls;
        while conn.is_handshaking() {
            let (read, wrote) = conn.complete_io(sock).map_err(|error| {
                // Keep the revocation and deadline kinds callers match on;
                // wrap only real TLS failures.
                match error.kind() {
                    io::ErrorKind::PermissionDenied | io::ErrorKind::TimedOut => error,
                    // The kind is kept too: a TLS alert or a refused
                    // certificate is `InvalidData`, which a caller reads as a
                    // TLS failure rather than a connect one.
                    kind => io::Error::new(kind, format!("TLS handshake failed: {error}")),
                }
            })?;
            // Neither direction moving mid-handshake means the peer went away;
            // testing only writes would spin against a peer that went silent.
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

    /// Take the raw socket back out of a plaintext stream, for the TLS handshake
    /// after a proxy `CONNECT`. `None` under TLS, whose session state it would
    /// discard.
    #[must_use]
    pub fn into_tcp(self) -> Option<TcpStream> {
        match self.inner {
            Inner::Plain(bounded) => Some(bounded.sock),
            Inner::Tls(_) => None,
        }
    }

    /// Carry `deadline` from here on: the request's own, once the connect
    /// phase's budget ([`Deadline::connect_phase`]) has done its work.
    pub(crate) fn set_deadline(&mut self, deadline: Deadline) {
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

// Checked here as well as before every socket call: rustls can answer a read
// from plaintext it holds, or buffer a write, without touching the socket.
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
/// close, or an error. rustls's reader reports a close without
/// `close_notify` as `UnexpectedEof`, as its own `read` does.
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
        // THE linearization point: request data does not leave this process
        // unless authority still holds right here (and, under TLS, again before
        // each socket write that carries it).
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
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A wait for something that must happen is a hang detector, never a latency budget.
    const HANG_DETECTOR: Duration = Duration::from_secs(60);

    #[derive(Debug)]
    struct Revocable(AtomicBool);

    impl Guard for Revocable {
        fn is_authorized(&self) -> bool {
            self.0.load(Ordering::Acquire)
        }
    }

    fn connected_pair() -> (TcpStream, TcpStream) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let server = listener.accept().unwrap().0;
        (client, server)
    }

    #[test]
    fn plaintext_io_sets_only_its_socket_timeout_direction() {
        let (read_client, mut read_server) = connected_pair();
        read_server.write_all(b"r").unwrap();
        let mut read_stream = Stream::plain(
            read_client,
            Arc::new(AlwaysAuthorized),
            Deadline::after(HANG_DETECTOR),
        );
        let mut byte = [0_u8; 1];
        read_stream.read_exact(&mut byte).unwrap();
        let read_client = read_stream.into_tcp().unwrap();
        assert!(read_client.read_timeout().unwrap().is_some());
        assert_eq!(read_client.write_timeout().unwrap(), None);

        let (write_client, _write_server) = connected_pair();
        let mut write_stream = Stream::plain(
            write_client,
            Arc::new(AlwaysAuthorized),
            Deadline::after(HANG_DETECTOR),
        );
        write_stream.write_all(b"w").unwrap();
        write_stream.flush().unwrap();
        let write_client = write_stream.into_tcp().unwrap();
        assert_eq!(write_client.read_timeout().unwrap(), None);
        assert!(write_client.write_timeout().unwrap().is_some());
    }

    #[test]
    fn a_revoked_guard_stops_writes_and_reads_on_an_open_socket() {
        // The socket is healthy; only authority changed.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let accepted = std::thread::spawn(move || listener.accept().unwrap().0);
        let client = TcpStream::connect(addr).unwrap();
        let _server = accepted.join().unwrap();

        let guard = Arc::new(Revocable(AtomicBool::new(true)));
        let mut stream = Stream::plain(
            client,
            Arc::clone(&guard) as Arc<dyn Guard>,
            Deadline::after(HANG_DETECTOR),
        );
        assert!(stream.write(b"hello").is_ok());

        guard.0.store(false, Ordering::Release);
        let error = stream.write(b"secret").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let mut buf = [0u8; 4];
        assert_eq!(
            stream.read(&mut buf).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    /// Apple platforms refuse every `setsockopt` (EINVAL) on a socket a peer
    /// reset, yet the bytes the peer sent before the reset stay readable: an
    /// early answer (a 401 before the request was read) is read whole, and
    /// only then the reset, never an invalid argument.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn an_answer_sent_before_a_reset_is_read_and_then_the_reset() {
        let (mut client, mut server) = connected_pair();
        // Both hang detectors are set while the sockets are whole: after the
        // reset no setter call can succeed.
        client.set_read_timeout(Some(HANG_DETECTOR)).unwrap();
        server.set_read_timeout(Some(HANG_DETECTOR)).unwrap();
        client.write_all(b"unread").unwrap();
        let mut byte = [0_u8; 1];
        assert_eq!(server.peek(&mut byte).unwrap(), 1);
        let answer: &[u8] = b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\n\r\nno";
        server.write_all(answer).unwrap();
        // Closing over bytes it never read makes the server send RST.
        drop(server);
        // The reset has landed once a setter fails; nothing is read meanwhile,
        // so neither the answer nor the pending reset is consumed here.
        let waited = Instant::now();
        while client.set_read_timeout(Some(HANG_DETECTOR)).is_ok() {
            assert!(waited.elapsed() < HANG_DETECTOR, "the reset never landed");
            std::thread::yield_now();
        }

        let mut stream = Stream::plain(
            client,
            Arc::new(AlwaysAuthorized),
            Deadline::after(HANG_DETECTOR),
        );
        let mut got = Vec::new();
        let mut buf = [0_u8; 256];
        let end = loop {
            match stream.read(&mut buf) {
                Ok(0) => break None,
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(error) => break Some(error),
            }
        };
        assert_eq!(got, answer);
        let error = end.expect("the reset after the answer, not an EOF");
        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset, "{error:?}");
    }

    #[test]
    fn a_resolver_that_never_answers_is_a_dns_timeout() {
        // The resolver blocks on a channel nobody sends on, as a resolver
        // wedged on an unreachable DNS server does.
        let (never_tx, never_rx) = mpsc::channel::<()>();
        let error = resolve_within(
            "example.invalid",
            443,
            Duration::from_millis(100),
            move |_, _| {
                let _ = never_rx.recv_timeout(HANG_DETECTOR);
                Ok(std::iter::empty())
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "dns: no answer within 100ms");
        // Ending the wait lets the helper finish; its late answer has nowhere to go.
        drop(never_tx);
    }

    #[test]
    fn a_failed_lookup_keeps_its_kind_and_names_the_host() {
        let error = resolve_within("example.invalid", 443, HANG_DETECTOR, |_, _| {
            Err::<std::iter::Empty<SocketAddr>, _>(io::Error::new(
                io::ErrorKind::NotFound,
                "no such host",
            ))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(
            error.to_string().starts_with("dns: example.invalid:"),
            "{error}"
        );

        // If helper creation is refused, the same path must fail without
        // invoking a resolver that could block indefinitely on this thread.
        let called = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&called);
        let error = resolve_within_using(
            "example.invalid",
            443,
            HANG_DETECTOR,
            move |_, _| {
                observed.store(true, Ordering::SeqCst);
                Ok(std::iter::empty::<SocketAddr>())
            },
            |_lookup| {
                Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "no helper threads available",
                ))
            },
        )
        .unwrap_err();
        assert!(!called.load(Ordering::SeqCst));
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(
            error.to_string().starts_with("dns: example.invalid:"),
            "{error}"
        );
    }

    #[test]
    fn a_resolved_address_connects() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let addrs = resolve_within("loopback.test", addr.port(), HANG_DETECTOR, move |_, _| {
            Ok(std::iter::once(addr))
        })
        .unwrap();
        assert_eq!(addrs, vec![addr]);
        let stream = TcpStream::connect_timeout(&addrs[0], HANG_DETECTOR).unwrap();
        assert_eq!(stream.peer_addr().unwrap(), addr);
        drop(listener.accept().unwrap());
    }

    #[test]
    fn an_idle_cap_shorter_than_the_budget_bounds_each_call_and_names_itself() {
        let deadline = Deadline::after(HANG_DETECTOR).with_idle(Duration::from_millis(200));
        assert_eq!(deadline.idle(), Some(Duration::from_millis(200)));
        let (budget, cap) = deadline.per_call().unwrap();
        assert_eq!(budget, Duration::from_millis(200));
        assert_eq!(deadline.expired(cap).to_string(), "no bytes for 200ms");
        // An idle longer than what remains is no cap: the total is.
        let short = Deadline::after(HANG_DETECTOR).with_idle(2 * HANG_DETECTOR);
        let (budget, cap) = short.per_call().unwrap();
        assert!(budget <= HANG_DETECTOR);
        assert_eq!(cap, Cap::Total);
        // The total's expiry is a timeout that is not the idle text agh
        // classifies by: which bound fired is `cap`, pinned above.
        let expired = short.expired(cap);
        assert_eq!(expired.kind(), io::ErrorKind::TimedOut);
        assert!(
            !expired.to_string().starts_with("no bytes for"),
            "{expired}"
        );
        assert_eq!(Deadline::after(HANG_DETECTOR).idle(), None);
    }

    #[test]
    fn the_connect_phase_takes_the_sooner_end() {
        let total = Deadline::after(2 * HANG_DETECTOR);
        let connect = total.connect_phase(HANG_DETECTOR);
        assert!(
            connect
                .remaining()
                .is_some_and(|left| left <= HANG_DETECTOR)
        );
        assert_eq!(connect.budget, Budget::Connect);
        assert_eq!(connect.expired(Cap::Total).kind(), io::ErrorKind::TimedOut);
        // A connect budget past the total's end leaves the total in charge.
        let kept = Deadline::after(HANG_DETECTOR).connect_phase(2 * HANG_DETECTOR);
        assert_eq!(kept.budget, Budget::Request);
    }

    #[test]
    fn a_budget_too_long_for_the_clock_is_far_off_rather_than_a_panic() {
        let unbounded = Deadline::after(Duration::MAX);
        assert!(
            unbounded
                .remaining()
                .is_some_and(|left| left > HANG_DETECTOR)
        );
        let connect = Deadline::after(HANG_DETECTOR).connect_phase(Duration::MAX);
        assert_eq!(connect.budget, Budget::Request);
        let connect = unbounded.connect_phase(Duration::MAX);
        assert!(connect.remaining().is_some_and(|left| left > HANG_DETECTOR));
    }

    #[test]
    fn an_expired_deadline_is_a_timeout_not_a_hang() {
        let deadline = Deadline::after(Duration::ZERO);
        assert!(deadline.remaining().is_none());
        assert_eq!(
            deadline.remaining_or_timeout().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }
}
