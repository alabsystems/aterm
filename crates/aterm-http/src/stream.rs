// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The byte stream under the HTTP layer: TCP, optionally wrapped in TLS, with
//! a global deadline and a revocable authority checked on every I/O step.
//!
//! The title-summary worker's authority can be revoked from the UI thread while
//! a request is in flight, and DNS, `connect`, proxy negotiation and the TLS
//! handshake can each block for seconds, so one check at the top is nearly
//! meaningless. [`Guard`] is re-checked at every read and write: the points
//! where terminal context could leave the process or a response be admitted.
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
#[derive(Clone, Copy, Debug, Default)]
pub struct TcpConnector;

impl Connect for TcpConnector {
    fn connect(&self, host: &str, port: u16, deadline: Deadline) -> io::Result<TcpStream> {
        use std::net::ToSocketAddrs;
        // `to_socket_addrs` blocks with no timeout of its own: an expired
        // request must not enter it.
        deadline.remaining_or_timeout()?;
        let addrs = (host, port).to_socket_addrs()?;
        let mut last = None;
        for addr in addrs {
            // Re-derive per address, or the global timeout is a per-address one.
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
    fn admit(&self) -> io::Result<Duration> {
        if !self.guard.is_authorized() {
            return Err(revoked_error());
        }
        self.deadline.remaining_or_timeout()
    }

    /// [`Self::admit`], then the remaining budget as the read timeout. The
    /// budget only shrinks, so no call outlives the deadline by more than itself.
    fn admit_read(&self) -> io::Result<()> {
        let budget = self.admit()?;
        self.sock.set_read_timeout(Some(budget))
    }

    /// As [`Self::admit_read`], for a write.
    fn admit_write(&self) -> io::Result<()> {
        let budget = self.admit()?;
        self.sock.set_write_timeout(Some(budget))
    }
}

/// Normalise a socket-timeout error into the deadline error.
///
/// `SO_RCVTIMEO` reports `WouldBlock` on Unix and `TimedOut` on Windows; the
/// only timeout this socket carries is the remaining global budget, so both
/// mean the deadline. It matters inside `complete_io` too, which treats
/// `WouldBlock` as "come back later".
fn as_deadline_timeout(error: io::Error) -> io::Error {
    match error.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
            io::Error::new(io::ErrorKind::TimedOut, "request deadline exceeded")
        }
        _ => error,
    }
}

impl Read for Bounded {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.admit_read()?;
        self.sock.read(buf).map_err(as_deadline_timeout)
    }
}

impl Write for Bounded {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.admit_write()?;
        self.sock.write(buf).map_err(as_deadline_timeout)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.admit_write()?;
        self.sock.flush().map_err(as_deadline_timeout)
    }
}

/// A TCP stream, optionally under TLS, carrying the deadline and the guard.
pub struct Stream {
    inner: Inner,
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
                    _ => io::Error::other(format!("TLS handshake failed: {error}")),
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
            Inner::Tls(tls) => tls.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // THE linearization point: request data does not leave this process
        // unless authority still holds right here (and, under TLS, again before
        // each socket write that carries it).
        self.socket().admit()?;
        match &mut self.inner {
            Inner::Plain(bounded) => bounded.write(buf),
            Inner::Tls(tls) => tls.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.socket().admit()?;
        match &mut self.inner {
            Inner::Plain(bounded) => bounded.flush(),
            Inner::Tls(tls) => tls.flush(),
        }
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
