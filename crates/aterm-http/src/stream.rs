// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deadline-bounded TCP/TLS byte stream with revocable [`Guard`] authority checked per I/O step.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

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
    /// Creates a deadline `budget` from now with no idle cap.
    #[must_use]
    pub fn after(budget: Duration) -> Self {
        Self {
            at: later(Instant::now(), budget),
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
        use std::net::ToSocketAddrs;
        connect_with(
            host,
            port,
            deadline,
            |host, port| (host, port).to_socket_addrs(),
            crate::local_network::denied,
        )
    }
}

/// [`TcpConnector::connect`] over `resolver`, a failed lookup asking `denied`
/// ([`refused_local`]).
fn connect_with<F, I>(
    host: &str,
    port: u16,
    deadline: Deadline,
    resolver: F,
    denied: impl FnOnce(&str, Duration) -> bool,
) -> io::Result<TcpStream>
where
    F: FnOnce(&str, u16) -> io::Result<I> + Send + 'static,
    I: Iterator<Item = SocketAddr>,
{
    let within = deadline.remaining_or_timeout()?;
    let addrs = resolve_within(host, port, within, resolver)
        .map_err(|error| refused_local(host, error, denial_wait(&deadline), denied))?;
    let mut last = None;
    for addr in addrs {
        let (budget, cap) = deadline.per_call()?;
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

/// How long a failed lookup of a local name waits for mDNSResponder to say
/// whether it refuses this process the name ([`refused_local`]).
const DENIAL_WAIT: Duration = Duration::from_secs(1);

/// [`DENIAL_WAIT`], or what is left of `deadline` when less (nothing once it
/// has passed).
fn denial_wait(deadline: &Deadline) -> Duration {
    deadline
        .remaining()
        .map_or(Duration::ZERO, |left| left.min(DENIAL_WAIT))
}

/// `error`, a failed lookup of `host`, named as macOS's Local Network refusal
/// when `denied` says mDNSResponder refuses this process the local name within
/// `wait` (`crate::local_network`). `getaddrinfo` reports that refusal as a
/// definite "no such name", read as "nothing there"; a connect macOS refuses
/// the same app fails `HostUnreachable`, so the lookup's refusal takes that
/// kind too. A lookup that timed out (a question still on screen holds it) and
/// a name the gate does not cover are left as they are, without asking.
fn refused_local(
    host: &str,
    error: io::Error,
    wait: Duration,
    denied: impl FnOnce(&str, Duration) -> bool,
) -> io::Error {
    if error.kind() == io::ErrorKind::TimedOut
        || wait.is_zero()
        || !crate::local_network::is_local_name(host)
        || !denied(host, wait)
    {
        return error;
    }
    io::Error::new(
        io::ErrorKind::HostUnreachable,
        format!(
            "dns: {host}: macOS does not let this app use the local network (Local Network privacy)"
        ),
    )
}

/// Resolve `host:port` through `resolver` on a helper thread, waiting at most
/// `within` for its answer.
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

impl Stream {
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
        client.set_read_timeout(Some(HANG_DETECTOR)).unwrap();
        server.set_read_timeout(Some(HANG_DETECTOR)).unwrap();
        client.write_all(b"unread").unwrap();
        let mut byte = [0_u8; 1];
        assert_eq!(server.peek(&mut byte).unwrap(), 1);
        let answer: &[u8] = b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\n\r\nno";
        server.write_all(answer).unwrap();
        drop(server);
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
        drop(never_tx);
    }

    /// A local name mDNSResponder refuses is named as macOS's refusal, the kind
    /// a refused connect has; a timeout, a name the gate does not cover, no
    /// time left and no refusal keep the lookup's own error, and only a local
    /// name with time left is asked about.
    #[test]
    fn only_a_refused_local_name_is_named_as_the_local_network_refusal() {
        let no_such_name =
            || io::Error::other("dns: alab-secrets.local: nodename nor servname provided");
        let wait = Duration::from_secs(1);
        let asked = std::cell::RefCell::new(Vec::new());
        let refuses = |host: &str, within: Duration| {
            asked.borrow_mut().push((host.to_owned(), within));
            true
        };
        let error = refused_local("alab-secrets.local", no_such_name(), wait, refuses);
        assert_eq!(error.kind(), io::ErrorKind::HostUnreachable);
        assert_eq!(
            error.to_string(),
            "dns: alab-secrets.local: macOS does not let this app use the local network (Local Network privacy)"
        );
        assert_eq!(*asked.borrow(), [("alab-secrets.local".to_owned(), wait)]);

        let held = io::Error::new(io::ErrorKind::TimedOut, "dns: no answer within 8000ms");
        for (host, error, wait) in [
            ("alab-secrets.local", held, wait),
            ("github.com", no_such_name(), wait),
            ("alab-secrets.local", no_such_name(), Duration::ZERO),
        ] {
            let kind = error.kind();
            let text = error.to_string();
            let kept = refused_local(host, error, wait, refuses);
            assert_eq!((kept.kind(), kept.to_string()), (kind, text), "{host}");
        }
        assert_eq!(asked.borrow().len(), 1, "none of those asks mDNSResponder");

        let kept = refused_local("alab-secrets.local", no_such_name(), wait, |_, _| false);
        assert_eq!(kept.kind(), io::ErrorKind::Other);
        assert!(kept.to_string().contains("nodename nor servname"), "{kept}");
    }

    /// The wait to ask is [`DENIAL_WAIT`] with time to spare, never more than
    /// the deadline has left, and nothing once it has passed.
    #[test]
    fn the_wait_to_ask_is_capped_by_what_the_deadline_has_left() {
        assert_eq!(denial_wait(&Deadline::after(HANG_DETECTOR)), DENIAL_WAIT);
        let short = DENIAL_WAIT / 4;
        assert!(denial_wait(&Deadline::after(short)) <= short);
        assert_eq!(
            denial_wait(&Deadline::after(Duration::ZERO)),
            Duration::ZERO
        );
    }

    /// The connector's failed lookup goes through that naming, with at most
    /// [`DENIAL_WAIT`] to ask; CONTROL: no refusal keeps the lookup's error.
    #[test]
    fn the_connector_names_a_refused_local_lookup() {
        let lookup = |_: &str, _: u16| {
            Err::<std::iter::Empty<SocketAddr>, _>(io::Error::other(
                "nodename nor servname provided",
            ))
        };
        let waits = std::cell::RefCell::new(Vec::new());
        let error = connect_with(
            "alab-secrets.local",
            7484,
            Deadline::after(HANG_DETECTOR),
            lookup,
            |_, within| {
                waits.borrow_mut().push(within);
                true
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::HostUnreachable, "{error}");
        assert_eq!(*waits.borrow(), [DENIAL_WAIT]);

        let error = connect_with(
            "alab-secrets.local",
            7484,
            Deadline::after(HANG_DETECTOR),
            lookup,
            |_, _| false,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other, "{error}");
        assert!(
            error
                .to_string()
                .starts_with("dns: alab-secrets.local: nodename"),
            "{error}"
        );
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
        let short = Deadline::after(HANG_DETECTOR).with_idle(2 * HANG_DETECTOR);
        let (budget, cap) = short.per_call().unwrap();
        assert!(budget <= HANG_DETECTOR);
        assert_eq!(cap, Cap::Total);
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
