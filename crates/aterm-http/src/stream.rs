// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The byte stream under the HTTP layer: TCP, optionally wrapped in TLS, with
//! a global deadline and a revocable authority checked on every I/O step.
//!
//! # The authority guard
//!
//! The title-summary worker can have its permission to speak revoked from the
//! UI thread WHILE a request is in flight (the user closes the tab, disables
//! the feature, or the session's authority epoch moves). DNS, `connect`, proxy
//! negotiation and the TLS handshake can all block for seconds before a single
//! body byte moves, so checking authority once at the top would be almost
//! meaningless.
//!
//! [`Guard`] is therefore re-checked at every read and every write, which are
//! the points at which terminal context could actually leave this process or a
//! response could be admitted. The check is two atomic loads, so revocation on
//! the UI thread stays wait-free. This is the same linearization point the
//! retired client's `Transport` wrapper used, now expressed directly in the
//! write loop instead of through a foreign trait.
//!
//! # Deadlines
//!
//! One deadline covers the whole request, matching the previous client's
//! `timeout_global`. It is converted to a per-syscall socket timeout before
//! each operation, so a peer that trickles bytes cannot extend the total.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
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

/// A single wall-clock budget for an entire request.
#[derive(Clone, Copy, Debug)]
pub struct Deadline {
    at: Instant,
}

impl Deadline {
    /// A deadline `budget` from now.
    #[must_use]
    pub fn after(budget: Duration) -> Self {
        Self {
            at: Instant::now() + budget,
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
        self.remaining()
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "request deadline exceeded"))
    }
}

/// Opens the TCP connection a request will run over.
///
/// This is the seam the managed-Ollama path replaces: it connects to a socket
/// address pinned in advance and attests the peer process on the ESTABLISHED
/// four-tuple before any request byte is written, which a hostname-based
/// connect could not do.
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
#[derive(Clone, Copy, Debug, Default)]
pub struct TcpConnector;

impl Connect for TcpConnector {
    fn connect(&self, host: &str, port: u16, deadline: Deadline) -> io::Result<TcpStream> {
        use std::net::ToSocketAddrs;
        // Check the budget BEFORE resolving: `to_socket_addrs` is a blocking
        // system call with no timeout of its own, so an already-expired request
        // must not enter it.
        deadline.remaining_or_timeout()?;
        let addrs = (host, port).to_socket_addrs()?;
        let mut last = None;
        for addr in addrs {
            // Re-derive per address: three dead addresses must not each get the
            // full budget, or the global timeout would be a per-address one.
            let budget = deadline.remaining_or_timeout()?;
            match TcpStream::connect_timeout(&addr, budget) {
                Ok(stream) => {
                    stream.set_nodelay(true)?;
                    return Ok(stream);
                }
                Err(error) => last = Some(error),
            }
        }
        Err(last.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "host resolved to no addresses")
        }))
    }
}

/// A TCP stream, optionally under TLS, carrying the deadline and the guard.
pub struct Stream {
    inner: Inner,
    guard: Arc<dyn Guard>,
    deadline: Deadline,
}

enum Inner {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IoDirection {
    Read,
    Write,
}

fn timeout_directions(is_tls: bool, operation: IoDirection) -> (bool, bool) {
    // A rustls read may need to write protocol records and a write may first
    // need to read them, so TLS keeps both socket directions bounded. Plain TCP
    // has no such cross-direction I/O and should pay only the relevant syscall.
    (
        is_tls || operation == IoDirection::Read,
        is_tls || operation == IoDirection::Write,
    )
}

impl Stream {
    /// Wrap an established plaintext connection.
    #[must_use]
    pub fn plain(tcp: TcpStream, guard: Arc<dyn Guard>, deadline: Deadline) -> Self {
        Self {
            inner: Inner::Plain(tcp),
            guard,
            deadline,
        }
    }

    /// Complete a TLS handshake over an established connection.
    ///
    /// `server_name` is the identity the certificate is checked against — the
    /// ORIGIN host, even when the bytes travel through a proxy tunnel, so a
    /// proxy cannot substitute its own certificate.
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
        let mut stream = Self {
            inner: Inner::Tls(Box::new(rustls::StreamOwned::new(connection, tcp))),
            guard,
            deadline,
        };
        // Drive the handshake now so a certificate failure surfaces here rather
        // than as a confusing short write later.
        // The budget is re-derived INSIDE the loop, not once before it. One
        // `complete_io` performs many `read_tls` syscalls, and a socket timeout is
        // PER-SYSCALL — so applying it once handed every one of those reads the
        // full remaining budget, and a peer trickling a byte at a time reset it
        // forever. That directly contradicted this module's own promise that
        // "one deadline covers the whole request". `remaining_or_timeout` shrinks
        // as the deadline approaches and returns TimedOut once it is spent, so
        // the total is now genuinely bounded across the handshake.
        let Self {
            inner,
            guard,
            deadline,
        } = &mut stream;
        if let Inner::Tls(tls) = inner {
            while tls.conn.is_handshaking() {
                if !guard.is_authorized() {
                    return Err(revoked_error());
                }
                let budget = deadline.remaining_or_timeout()?;
                tls.sock.set_read_timeout(Some(budget))?;
                tls.sock.set_write_timeout(Some(budget))?;
                let (read, wrote) = tls
                    .conn
                    .complete_io(&mut tls.sock)
                    .map_err(|error| io::Error::other(format!("TLS handshake failed: {error}")))?;
                // NEITHER direction moving while still handshaking means the
                // peer went away; surface it rather than spin on a dead socket.
                // Testing only the write side would loop forever against a peer
                // that accepts our flight and then stops talking.
                if read == 0 && wrote == 0 && tls.conn.is_handshaking() {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "peer closed during TLS handshake",
                    ));
                }
            }
        }
        Ok(stream)
    }

    /// Whether this stream is under TLS.
    #[must_use]
    pub fn is_tls(&self) -> bool {
        matches!(self.inner, Inner::Tls(_))
    }

    /// Take the raw socket back out of a PLAINTEXT stream.
    ///
    /// Used once, by the proxy path: after `CONNECT` succeeds the same socket
    /// has to be handed to the TLS handshake. `None` for a stream already under
    /// TLS — unwrapping an established TLS session would discard its state.
    #[must_use]
    pub fn into_tcp(self) -> Option<TcpStream> {
        match self.inner {
            Inner::Plain(tcp) => Some(tcp),
            Inner::Tls(_) => None,
        }
    }

    /// Push the remaining deadline down only to socket directions this
    /// operation can use. TLS may perform cross-direction I/O, unlike plain TCP.
    fn apply_timeouts(&mut self, operation: IoDirection) -> io::Result<()> {
        let budget = self.deadline.remaining_or_timeout()?;
        let (tcp, is_tls) = match &self.inner {
            Inner::Plain(tcp) => (tcp, false),
            Inner::Tls(tls) => (&tls.sock, true),
        };
        let (set_read, set_write) = timeout_directions(is_tls, operation);
        if set_read {
            tcp.set_read_timeout(Some(budget))?;
        }
        if set_write {
            tcp.set_write_timeout(Some(budget))?;
        }
        Ok(())
    }

    /// Guard + deadline check performed before every read and write.
    fn admit(&mut self, operation: IoDirection) -> io::Result<()> {
        if !self.guard.is_authorized() {
            return Err(revoked_error());
        }
        self.apply_timeouts(operation)
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.admit(IoDirection::Read)?;
        match &mut self.inner {
            Inner::Plain(tcp) => tcp.read(buf),
            Inner::Tls(tls) => tls.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // THE linearization point: terminal context does not leave this process
        // unless authority still holds right here.
        self.admit(IoDirection::Write)?;
        match &mut self.inner {
            Inner::Plain(tcp) => tcp.write(buf),
            Inner::Tls(tls) => tls.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.admit(IoDirection::Write)?;
        match &mut self.inner {
            Inner::Plain(tcp) => tcp.flush(),
            Inner::Tls(tls) => tls.flush(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

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
    fn timeout_direction_classification_keeps_tls_bidirectional() {
        assert_eq!(timeout_directions(false, IoDirection::Read), (true, false));
        assert_eq!(timeout_directions(false, IoDirection::Write), (false, true));
        assert_eq!(timeout_directions(true, IoDirection::Read), (true, true));
        assert_eq!(timeout_directions(true, IoDirection::Write), (true, true));
    }

    #[test]
    fn plaintext_io_sets_only_its_socket_timeout_direction() {
        let (read_client, mut read_server) = connected_pair();
        read_server.write_all(b"r").unwrap();
        let mut read_stream = Stream::plain(
            read_client,
            Arc::new(AlwaysAuthorized),
            Deadline::after(Duration::from_secs(5)),
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
            Deadline::after(Duration::from_secs(5)),
        );
        write_stream.write_all(b"w").unwrap();
        write_stream.flush().unwrap();
        let write_client = write_stream.into_tcp().unwrap();
        assert_eq!(write_client.read_timeout().unwrap(), None);
        assert!(write_client.write_timeout().unwrap().is_some());
    }

    #[test]
    fn a_revoked_guard_stops_writes_and_reads_on_an_open_socket() {
        // The socket is healthy; only authority changed. This is the case a
        // top-of-request check would miss.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let accepted = std::thread::spawn(move || listener.accept().unwrap().0);
        let client = TcpStream::connect(addr).unwrap();
        let _server = accepted.join().unwrap();

        let guard = Arc::new(Revocable(AtomicBool::new(true)));
        let mut stream = Stream::plain(
            client,
            Arc::clone(&guard) as Arc<dyn Guard>,
            Deadline::after(Duration::from_secs(5)),
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

    #[test]
    fn an_expired_deadline_is_a_timeout_not_a_hang() {
        let deadline = Deadline::after(Duration::from_millis(0));
        std::thread::sleep(Duration::from_millis(2));
        assert!(deadline.remaining().is_none());
        assert_eq!(
            deadline.remaining_or_timeout().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn a_live_deadline_reports_a_positive_budget() {
        let deadline = Deadline::after(Duration::from_secs(30));
        assert!(deadline.remaining().unwrap() > Duration::from_secs(20));
    }
}
