// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TLS 1.3 transport (rustls + the `ring` provider) for the fleet
//! ([`crate::fleet`]).
//!
//! TLS gives three things the local Unix socket got for free: (1) an encrypted
//! channel; (2) server identity via **certificate fingerprint pinning** (a
//! custom [`ServerCertVerifier`] — no CA/PKI: the fleet's dialer pins the one
//! certificate every aterm embeds, and still verifies the handshake signature);
//! and (3) the **RFC 5705 keying-material exporter** — 32 bytes unique to this
//! TLS session, symmetric on both ends, that the fleet's proofs are taken over.
//! The exporter is what makes a proof resist an active MITM: a relay that
//! terminates one TLS leg holds a *different* exporter, so a captured tag never
//! transfers. [`relay_answers`] then bridges a proved stream to a local end
//! that answers, the fleet listener's control server, and [`answer`] answers
//! one on the stream itself, in the same order.
//!
//! Tests use a self-signed fixture under `src/testdata/`.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use aterm_uds::CtlStream;

use aterm_digest::Sha256;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme, StreamOwned};

/// The exporter length: what every proof over a channel is taken over.
pub const EXPORTER_LEN: usize = 32;

/// The single place this crate names a rustls `CryptoProvider` implementation.
fn provider() -> rustls::crypto::CryptoProvider {
    rustls::crypto::ring::default_provider()
}

/// Install `provider()` as the process default, once. Idempotent — a second call
/// (or a pre-installed provider) is ignored.
pub fn init_crypto() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Ignore the error: another caller may have already installed a provider.
        let _ = provider().install_default();
    });
}

/// SHA-256 of a certificate's DER — the fingerprint a dialer pins.
#[must_use]
pub fn cert_fingerprint(cert_der: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(cert_der);
    h.finalize()
}

fn io_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// Verifies the server cert by SHA-256 FINGERPRINT pinning (no CA/PKI), and the
/// handshake signature via the provider's algorithms (proving key possession).
/// The fleet's dialer ([`crate::fleet`]) pins its one embedded certificate with it.
#[derive(Debug)]
pub(crate) struct PinnedServerVerifier {
    pin: [u8; 32],
    supported: WebPkiSupportedAlgorithms,
}

impl PinnedServerVerifier {
    /// A verifier that accepts exactly the certificate whose DER hashes to `pin`,
    /// and checks every handshake signature with the `ring` provider's algorithms.
    pub(crate) fn new(pin: [u8; 32]) -> Self {
        Self {
            pin,
            supported: provider().signature_verification_algorithms,
        }
    }
}

impl ServerCertVerifier for PinnedServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // Constant-time-ish fingerprint compare (32 bytes, fixed length).
        let presented = cert_fingerprint(end_entity.as_ref());
        if presented == self.pin {
            Ok(ServerCertVerified::assertion())
        } else {
            // Both digests ride out in the error, so the dialer can say them.
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::Other(rustls::OtherError(Arc::new(PinMismatch {
                    presented,
                    pinned: self.pin,
                }))),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.supported)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.supported)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.supported.supported_schemes()
    }
}

/// A certificate whose SHA-256 is not the pinned one: what the remote presented
/// and what the connection pins. [`PinnedServerVerifier`] returns it inside
/// rustls' `CertificateError::Other`, and a dialer says it in these words
/// ([`dial_handshake_error`]).
#[derive(Debug)]
struct PinMismatch {
    presented: [u8; 32],
    pinned: [u8; 32],
}

impl std::fmt::Display for PinMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "certificate mismatch: the remote presents sha256:{}; the connection's fingerprint \
             is sha256:{}",
            aterm_codec::hex::encode_lower(&self.presented),
            aterm_codec::hex::encode_lower(&self.pinned)
        )
    }
}

impl std::error::Error for PinMismatch {}

/// A dialer's handshake error: a [`PinMismatch`] in its own words,
/// every other error as rustls gave it.
pub(crate) fn dial_handshake_error(e: io::Error) -> io::Error {
    let mismatch = e
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        .and_then(|error| match error {
            rustls::Error::InvalidCertificate(rustls::CertificateError::Other(other)) => {
                other.0.downcast_ref::<PinMismatch>()
            }
            _ => None,
        });
    match mismatch {
        Some(mismatch) => io::Error::new(io::ErrorKind::InvalidData, mismatch.to_string()),
        None => e,
    }
}

/// A listener's handshake error: a dialer's refusal of this
/// listener's certificate — the alert an aterm dialer sends when its pinned
/// fingerprint differs (`CertificateUnknown`, or `AccessDenied` from a dialer
/// older than [`PinMismatch`]) — in its own words, every other error as rustls
/// gave it. The words do not say why: any TLS client (a browser, a scanner)
/// sends the same alert for a self-signed certificate, and the aterm dialer's
/// own line gives both digests.
pub(crate) fn listen_handshake_error(e: io::Error) -> io::Error {
    let refused = e
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        .is_some_and(|error| {
            matches!(
                error,
                rustls::Error::AlertReceived(
                    rustls::AlertDescription::CertificateUnknown
                        | rustls::AlertDescription::AccessDenied
                )
            )
        });
    if refused {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "it refused this listener's certificate",
        )
    } else {
        e
    }
}

/// A connected TLS stream plus its channel exporter — what a fleet proof is
/// taken over. Generic over the rustls connection role
/// (`ServerConnection` / `ClientConnection`).
pub struct TlsTransport<C> {
    stream: StreamOwned<C, TcpStream>,
    exporter: [u8; EXPORTER_LEN],
}

impl<C> TlsTransport<C> {
    /// A transport over `stream`, whose handshake is complete, carrying the
    /// keying material its caller exported under its own label (the fleet's,
    /// [`crate::fleet`]).
    #[inline]
    pub(crate) const fn from_parts(
        stream: StreamOwned<C, TcpStream>,
        exporter: [u8; EXPORTER_LEN],
    ) -> Self {
        Self { stream, exporter }
    }

    /// The RFC 5705 keying material this connection's owner exported — 32
    /// bytes unique to this TLS session and identical on both ends, under the
    /// owner's own label (the fleet's, [`crate::fleet`]). It is what a proof
    /// covers, never its key: the fleet's role proofs are each keyed by their
    /// secret and taken over this value.
    #[inline]
    #[must_use]
    pub const fn exporter(&self) -> &[u8] {
        &self.exporter
    }

    /// The connected TLS stream (Read + Write) for the post-handshake relay.
    #[inline]
    pub const fn stream(&mut self) -> &mut StreamOwned<C, TcpStream> {
        &mut self.stream
    }

    /// Consume into the raw TLS stream.
    #[inline]
    pub fn into_stream(self) -> StreamOwned<C, TcpStream> {
        self.stream
    }

    /// The socket under the stream.
    #[inline]
    pub(crate) const fn tcp(&self) -> &TcpStream {
        &self.stream.sock
    }
}

/// The stream an [`answer`] is written on: its reads and writes, their
/// timeouts and its peer's address, and nothing that ends it — no
/// `close_notify`, no shutdown, no socket to take — so an answered
/// connection's only end is [`answer`]'s own, after its guard.
pub struct Answering<'a, C> {
    stream: &'a mut StreamOwned<C, TcpStream>,
}

impl<C> Answering<'_, C> {
    /// Bound each read of the request ([`TcpStream::set_read_timeout`]):
    /// nothing in [`answer`] waits on its own clock.
    ///
    /// # Errors
    /// The socket's own.
    #[inline]
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.sock.set_read_timeout(timeout)
    }

    /// Bound each write of the answer ([`TcpStream::set_write_timeout`]), for
    /// an answer written unasked to a peer that may read none of it.
    ///
    /// # Errors
    /// The socket's own.
    #[inline]
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.sock.set_write_timeout(timeout)
    }

    /// The address the peer dialled from.
    ///
    /// # Errors
    /// The socket's own.
    #[inline]
    pub fn peer_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.stream.sock.peer_addr()
    }

    /// The socket under the stream, for a test listener that writes raw
    /// records past TLS (a malformed one, dribbled).
    #[cfg(all(unix, test))]
    pub(crate) fn sock(&self) -> &TcpStream {
        &self.stream.sock
    }
}

impl<C, S> Read for Answering<'_, C>
where
    C: std::ops::DerefMut + std::ops::Deref<Target = rustls::ConnectionCommon<S>>,
    S: rustls::SideData,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buf)
    }
}

impl<C, S> Write for Answering<'_, C>
where
    C: std::ops::DerefMut + std::ops::Deref<Target = rustls::ConnectionCommon<S>>,
    S: rustls::SideData,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// Answer `transport` HERE, in [`relay_answers`]' order: `write` reads the
/// request and writes the answer on the stream, the answer is flushed, then
/// `guard` is dropped, and only then does the peer hear the end — when the
/// answer went cleanly a `close_notify` written only as far as the socket
/// takes it at once, then the FIN. So a peer that has read the end reads it
/// after `guard` is gone (the fleet listener's frees the connection's slot
/// and share), and nothing after `guard` waits on the peer. `write` gets an
/// [`Answering`], which cannot end the connection itself. Bound the
/// stream's reads before answering ([`Answering::set_read_timeout`]).
///
/// # Errors
/// `write`'s own, or the flush's.
pub fn answer<C, S, G, T>(
    mut transport: TlsTransport<C>,
    guard: G,
    write: impl FnOnce(&mut Answering<'_, C>) -> io::Result<T>,
) -> io::Result<T>
where
    C: std::ops::DerefMut + std::ops::Deref<Target = rustls::ConnectionCommon<S>>,
    S: rustls::SideData,
{
    let answered = write(&mut Answering {
        stream: transport.stream(),
    })
    .and_then(|t| transport.stream().flush().map(|()| t));
    drop(guard);
    end_without_waiting(transport, answered.is_ok());
    answered
}

/// End `transport` once its owner has answered, never waiting on the peer,
/// as [`relay_answers`] ends: when the answer went `clean`ly, a
/// `close_notify` written only as far as the socket takes it at once; then
/// the FIN.
fn end_without_waiting<C, S>(transport: TlsTransport<C>, clean: bool)
where
    C: std::ops::DerefMut + std::ops::Deref<Target = rustls::ConnectionCommon<S>>,
    S: rustls::SideData,
{
    let mut stream = transport.into_stream();
    let mut last = Vec::new();
    if clean {
        stream.conn.send_close_notify();
        while stream.conn.wants_write() {
            match stream.conn.write_tls(&mut last) {
                Ok(1..) => {}
                Ok(0) | Err(_) => break,
            }
        }
    }
    send_without_waiting(&stream.sock, &last);
    aterm_uds::hangup::hang_up(&stream.sock);
}

/// Upper bound on the uploader's final draining wait, so a stuck (TCP-connected
/// but non-reading) peer cannot hang relay teardown — far longer than any healthy
/// peer needs to drain a queued tail, short enough to bound the join.
const RELAY_DRAIN_MAX: Duration = Duration::from_secs(5);

/// The rustls connection plus relay coordination flags, guarded by one mutex and
/// signaled through one [`Condvar`] (see [`RelayShared`]). No thread performs
/// blocking I/O while holding the lock.
struct RelayState<C> {
    conn: C,
    /// Teardown: the answer is over, or a fatal error on either side.
    done: bool,
    /// The writer thread holds TLS bytes it extracted from `conn` but has not yet
    /// pushed to the socket — the uploader's final drain must wait for them too.
    inflight: bool,
    /// The first UNEXPECTED error from any direction (a normal peer close is not
    /// recorded — it is the expected end of the relay).
    err: Option<io::Error>,
    /// The relay's guard ([`relay_answers`]), taken out by the uploader when
    /// the answer is over and dropped there in its order. Kept here, never in the uploader's closure, so a relay whose
    /// uploader cannot start lets it go only once its writer has gone.
    guard: Option<Box<dyn Send>>,
}

impl<C> RelayState<C> {
    fn record_err(&mut self, e: io::Error) {
        if !is_normal_close(&e) && self.err.is_none() {
            self.err = Some(e);
        }
    }
}

#[cfg_attr(trust_verify, trust::paired)]
struct RelayShared<C> {
    state: Mutex<RelayState<C>>,
    /// Signals every relay condition: plaintext-buffer space freed, TLS output
    /// queued, in-flight write completed, teardown. Waiters re-check their own
    /// predicate under the lock, so shared notification is sound.
    cv: Condvar,
}

impl<C> RelayShared<C> {
    fn lock(&self) -> MutexGuard<'_, RelayState<C>> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Move every plaintext byte rustls has already decrypted into `plain`.
///
/// `Ok(true)` means the peer's stream ended (close_notify or a normal close);
/// `Ok(false)` means it is merely out of decrypted bytes for now. Decryption
/// happens when TLS records are fed in, so buffered plaintext can exist before
/// the relay has read a single byte itself.
///
/// `tmp` is a CALLER-OWNED staging buffer, allocated once per relay: a local
/// array would be zeroed on every call (its address escapes into
/// `reader().read`, so the optimizer cannot drop the memset), ~20 KiB of
/// zeroing per 16 KiB relayed. Reuse is safe: `read` never reads prior
/// contents and only `tmp[..k]` is consumed.
fn drain_decrypted<C, S>(conn: &mut C, plain: &mut Vec<u8>, tmp: &mut [u8]) -> io::Result<bool>
where
    C: std::ops::DerefMut + std::ops::Deref<Target = rustls::ConnectionCommon<S>>,
    S: rustls::SideData,
{
    loop {
        // The mutable borrow of `tmp` ends with the call, so the copy out below
        // sits after the match rather than inside an arm — same arms, same
        // ordering, same returns.
        let k = match conn.reader().read(tmp) {
            Ok(0) => return Ok(true),
            Ok(k) => k,
            Err(e) if is_would_block(&e) => return Ok(false),
            Err(e) if is_normal_close(&e) => return Ok(true),
            Err(e) => return Err(e),
        };
        plain.extend_from_slice(&tmp[..k]);
    }
}

/// Relay between an authenticated TLS stream and a local `CtlStream` whose
/// end ANSWERS — the fleet listener's control server, which answers a peer's
/// requests and then hangs up: the local end's end (its EOF, or an error)
/// ends the exchange both ways, and `guard` is dropped before the peer is
/// told. This is the network analog of `proxy.rs`'s local splice.
///
/// TLS cannot be split into owned read/write halves the way a `CtlStream` can
/// (the rustls `Connection` mixes both directions) — but the underlying TCP
/// socket CAN (`try_clone`), so the relay splits at the byte layer instead of
/// polling: the rustls connection lives behind a `Mutex` + `Condvar`
/// ([`RelayShared`]) and three threads do BLOCKING reads on their own socket
/// half, waking immediately on data and sleeping indefinitely when idle:
///
/// - **downloader** (this thread): blocking-reads TLS bytes off the TCP socket,
///   feeds them to rustls under the lock (`read_tls` + `process_new_packets`),
///   drains the decrypted plaintext, then writes it to the local socket with the
///   lock released.
/// - **uploader**: blocking-reads the local socket and buffers the plaintext into
///   rustls under the lock (`writer().write` encrypts into rustls' bounded
///   outgoing buffer; on `Ok(0)` — buffer at its limit — it condvar-waits for the
///   writer thread to free space).
/// - **writer**: condvar-waits for `wants_write()`, extracts the queued TLS bytes
///   under the lock (`write_tls` into a local buffer), then blocking-writes them
///   to the TCP socket with the lock released — so send-side backpressure never
///   holds the lock and never stalls the other direction.
///
/// No thread blocks while holding the lock, and teardown wakes blocked reads via
/// socket shutdown (plus a condvar broadcast for the waiters).
///
/// The order is the point. Every answer byte goes to the socket first, with
/// `guard` held: while the local end is still answering, a peer that does
/// not read holds its bytes for as long as it likes, until its connection
/// is cut (the fleet listener's lifetime, `fleet::SERVED_WITHIN`, 30 s);
/// once the local end's EOF is in, the drain of what is still queued waits
/// at most 5 s, so a peer that reads nothing cannot hold the join. Then the exchange is
/// over and nothing more is relayed either way; then the local end is hung
/// up and `guard` is dropped; and only then does the peer hear the end — a
/// `close_notify` written as far as the socket takes it at once, then the
/// FIN. So a peer that has read its connection's end reads it after `guard`
/// is gone (the fleet listener's guard frees the connection's slot and its
/// address's share, so that peer can dial again at once). Whatever waits on
/// the peer comes before `guard` goes, and nothing after it waits on either
/// end: what is left is that write, which never blocks, the FIN, and the
/// joins of threads the two hang-ups woke, so a relay that has let its guard
/// go holds its threads and descriptors for no time either end controls. An
/// exchange the PEER ends — its FIN or `close_notify`, a reset, or a fatal
/// error on its side (a record that does not decrypt, an alert) — is told
/// to the local end as an EOF after the line it is serving, never as a
/// hang-up, and ends the same way once the local end has answered that line
/// or hung up: the guard is held for as long as the local end serves what
/// it already read, so the thread serving it counts against the guard
/// however the peer ends. Once the exchange cannot reach the peer (a fatal
/// error, a write to the peer that fails, the listener's cut) nothing more
/// crosses: the local end's first answer byte after that ends it, and what
/// it writes after goes nowhere, never waiting.
///
/// The `close_notify` is therefore best effort: a peer whose receive window
/// is full when the answer is over may get the FIN without it, or without
/// all of it, and a read to its end then reports an unclean end
/// (`UnexpectedEof`) after every answer byte. A peer that reads its answer
/// by its own framing — every fleet reader reads lines — never sees the
/// difference.
///
/// Both hang-ups shut each half on its own ([`aterm_uds::hangup::hang_up`]),
/// never both ways at once: Darwin refuses a both-ways `shutdown` of a socket
/// whose read half is already shut — a peer that sent its FIN, a local end
/// that hung up — and shuts nothing, which would leave the thread writing to
/// that end blocked after the guard is gone.
///
/// # Errors
/// On setup failure (a socket clone, a thread that cannot start) or an
/// unexpected mid-stream I/O error (a normal peer close — EOF,
/// `close_notify`, a reset — is not an error).
pub fn relay_answers<C, S, G>(
    transport: TlsTransport<C>,
    local: CtlStream,
    guard: G,
) -> io::Result<()>
where
    C: std::ops::DerefMut + std::ops::Deref<Target = rustls::ConnectionCommon<S>> + Send + 'static,
    S: rustls::SideData + 'static,
    G: Send + 'static,
{
    let stream = transport.into_stream();
    let conn = stream.conn;
    let mut tcp_down = stream.sock; // blocking read: TLS bytes in (this thread)
    let tcp_wr = tcp_down.try_clone()?; // blocking write: TLS bytes out (writer thread)
    let tcp_up = tcp_down.try_clone()?; // uploader teardown: unblocks the TCP read

    let mut local_up = local.try_clone()?; // read local -> TLS plaintext
    let mut local_down = local; // write local <- TLS plaintext

    let shared = Arc::new(RelayShared {
        state: Mutex::new(RelayState {
            conn,
            done: false,
            inflight: false,
            err: None,
            guard: Some(Box::new(guard)),
        }),
        cv: Condvar::new(),
    });

    // Writer: rustls -> TCP. The only thread that extracts queued TLS bytes, so
    // record order is preserved; the socket write happens with the lock released.
    let wr = {
        let shared = Arc::clone(&shared);
        let mut tcp_wr = tcp_wr;
        let writer = std::thread::Builder::new().name("aterm-tls-out".to_owned());
        writer.spawn(move || {
            let mut out: Vec<u8> = Vec::with_capacity(16 * 1024);
            loop {
                let mut g = shared.lock();
                while !g.done && !g.conn.wants_write() {
                    g = shared.cv.wait(g).unwrap_or_else(|p| p.into_inner());
                }
                if g.done {
                    return;
                }
                out.clear();
                while g.conn.wants_write() {
                    match g.conn.write_tls(&mut out) {
                        Ok(1..) => {}
                        // A Vec sink cannot fail or stall; bail out defensively.
                        Ok(0) | Err(_) => break,
                    }
                }
                g.inflight = true;
                drop(g);
                let sent = tcp_wr.write_all(&out).and_then(|()| tcp_wr.flush());
                let mut g = shared.lock();
                g.inflight = false;
                if let Err(e) = sent {
                    if !g.done {
                        g.record_err(e);
                    }
                    g.done = true;
                    drop(g);
                    shared.cv.notify_all();
                    // Unblock the downloader's TCP read so it observes teardown.
                    aterm_uds::hangup::hang_up(&tcp_wr);
                    return;
                }
                drop(g);
                shared.cv.notify_all(); // buffer space freed / drain progressed
            }
        })
    };
    // A writer that cannot start ends the relay here, before any byte crossed:
    // the guard goes, then both sockets are hung up.
    let wr = match wr {
        Ok(wr) => wr,
        Err(e) => {
            let guard = shared.lock().guard.take();
            drop(guard);
            aterm_uds::hangup::hang_up(&tcp_down);
            aterm_uds::hangup::hang_up(&local_down);
            return Err(e);
        }
    };

    // Uploader: local -> rustls plaintext. Blocking local read (the local end
    // unblocks it by its answer or its own hang-up, teardown by `local_down`'s
    // hang-up); the lock is held only to buffer plaintext.
    let up = {
        let shared = Arc::clone(&shared);
        let uploader = std::thread::Builder::new().name("aterm-tls-in".to_owned());
        uploader.spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            // A local EOF is the answer's end: its tail is drained below and
            // the peer hears a clean end (`close_notify`). A fatal error ends
            // the exchange without one.
            let mut graceful = false;
            'up: loop {
                match local_up.read(&mut buf) {
                    Ok(0) => {
                        graceful = true;
                        break 'up; // local EOF: drain, then end the exchange
                    }
                    Ok(n) => {
                        let mut written = 0usize;
                        let mut g = shared.lock();
                        while written < n {
                            if g.done {
                                // Peer-side teardown already under way: to the
                                // end below, which the guard leaves only once
                                // neither socket can block.
                                break 'up;
                            }
                            match g.conn.writer().write(&buf[written..n]) {
                                // rustls' outgoing buffer is at its limit: wait
                                // for the writer thread to push it to the socket.
                                Ok(0) => {
                                    g = shared.cv.wait(g).unwrap_or_else(|p| p.into_inner());
                                }
                                Ok(k) => {
                                    written += k;
                                    shared.cv.notify_all(); // wake the writer thread
                                }
                                Err(e) => {
                                    g.record_err(e);
                                    break 'up;
                                }
                            }
                        }
                    }
                    Err(e) if is_would_block(&e) => {}
                    Err(e) if is_normal_close(&e) => {
                        graceful = true;
                        break 'up;
                    }
                    Err(e) => {
                        let mut g = shared.lock();
                        if !g.done {
                            g.record_err(e);
                        }
                        break 'up;
                    }
                }
            }
            // Final drain: wait (bounded, so a stuck — TCP-connected but
            // non-reading — peer cannot hang the join) until the writer thread
            // has pushed every queued TLS byte, so a response tail isn't
            // truncated. Skipped on downloader-initiated teardown (`done`: the
            // peer is already going away).
            let deadline = Instant::now() + RELAY_DRAIN_MAX;
            let mut g = shared.lock();
            while !g.done && (g.conn.wants_write() || g.inflight) {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                g = shared
                    .cv
                    .wait_timeout(g, deadline - now)
                    .unwrap_or_else(|p| p.into_inner())
                    .0;
            }
            // THE ANSWER IS OVER ([`relay_answers`]): nothing more crosses
            // either way. The close_notify is taken out of rustls here,
            // under the lock, so the writer thread never writes it — it
            // sees `done` and ends — and it is written below only as far
            // as the socket takes it at once. The local socket is hung
            // up before the guard goes, so the downloader, were it
            // blocked writing to a local end that has stopped reading,
            // returns; the FIN after it wakes the rest, a writer blocked
            // on a peer that sent its FIN and reads nothing included.
            // Nothing after the guard waits on either end.
            let clean = graceful && !g.done && !g.conn.wants_write() && !g.inflight;
            g.done = true;
            let guard = g.guard.take();
            let mut last = Vec::new();
            if clean {
                g.conn.send_close_notify();
                while g.conn.wants_write() {
                    match g.conn.write_tls(&mut last) {
                        Ok(1..) => {}
                        Ok(0) | Err(_) => break,
                    }
                }
            }
            drop(g);
            shared.cv.notify_all();
            aterm_uds::hangup::hang_up(&local_up);
            drop(guard);
            send_without_waiting(&tcp_up, &last);
            // The FIN, and every blocked read and write woken.
            aterm_uds::hangup::hang_up(&tcp_up);
        })
    };
    // An uploader that cannot start ends the relay here: the writer is told,
    // both sockets are hung up, and the guard goes only once the writer has.
    let up = match up {
        Ok(up) => up,
        Err(e) => {
            let guard = {
                let mut g = shared.lock();
                g.done = true;
                g.guard.take()
            };
            shared.cv.notify_all();
            aterm_uds::hangup::hang_up(&tcp_down);
            aterm_uds::hangup::hang_up(&local_down);
            let _ = wr.join();
            drop(guard);
            return Err(e);
        }
    };

    // Downloader: TCP -> rustls -> local (this thread). The blocking socket read
    // holds no lock; ingest + decrypt hold it only for buffer work.
    let mut tls_in = [0u8; 16 * 1024];
    let mut plain: Vec<u8> = Vec::with_capacity(16 * 1024);
    // Staging buffer `drain_decrypted` copies rustls' plaintext through, sized to
    // the same granularity as `tls_in`. Owned HERE, not by the callee, so the
    // whole relay pays its zero-init once instead of once per drain iteration
    // (see `drain_decrypted`); both drain sites below are on this one thread.
    let mut tls_plain = [0u8; 16 * 1024];
    let mut fatal_down = false;
    // This thread's own write to the local end failed: that end is gone, or
    // the uploader hung it up.
    let mut local_failed = false;

    // Drain before the first blocking read: a peer's first application record
    // can ride in the same TCP segment as the handshake's final flight, leaving
    // the first request already decrypted in rustls' plaintext buffer after
    // `complete_io`.
    let mut drained_eof = false;
    {
        let mut g = shared.lock();
        match drain_decrypted(&mut g.conn, &mut plain, &mut tls_plain) {
            Ok(eof) => drained_eof = eof,
            Err(e) => {
                g.record_err(e);
                g.done = true;
                fatal_down = true;
            }
        }
        if g.conn.wants_write() {
            shared.cv.notify_all();
        }
    }
    if !fatal_down
        && !plain.is_empty()
        && let Err(e) = local_down
            .write_all(&plain)
            .and_then(|()| local_down.flush())
    {
        let mut g = shared.lock();
        g.record_err(e);
        g.done = true;
        fatal_down = true;
        local_failed = true;
    }

    'down: loop {
        // The pre-loop drain already saw the peer's EOF (or failed): the
        // teardown below is the only thing left to run.
        if fatal_down || drained_eof {
            break;
        }
        let n = match tcp_down.read(&mut tls_in) {
            Ok(0) => break, // TCP EOF (with or without close_notify)
            Ok(n) => n,
            Err(e) if is_normal_close(&e) => break,
            Err(e) => {
                let mut g = shared.lock();
                if !g.done {
                    g.record_err(e);
                    g.done = true;
                }
                fatal_down = true;
                break;
            }
        };
        plain.clear();
        let mut clean_eof = false;
        {
            let mut g = shared.lock();
            if g.done {
                break;
            }
            let mut cursor: &[u8] = &tls_in[..n];
            // Drain after every `process_new_packets`: rustls holds at most
            // 16 KiB of decrypted plaintext and `read_tls` refuses input once
            // past it, while one socket read can complete more than 16 KiB.
            // A clean close ends the feed (`read_tls` reads nothing after a
            // `close_notify`).
            while !cursor.is_empty() && !clean_eof {
                match g.conn.read_tls(&mut cursor) {
                    Ok(1..) => {}
                    // A slice source cannot fail, and the drain below keeps
                    // the plaintext buffer empty before every feed, so
                    // no-progress here means a record rustls cannot hold:
                    // fatal, not retryable.
                    Ok(0) | Err(_) => {
                        g.record_err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "TLS ingest made no progress",
                        ));
                        g.done = true;
                        fatal_down = true;
                        break 'down;
                    }
                }
                if let Err(e) = g.conn.process_new_packets() {
                    g.record_err(io_err(e));
                    g.done = true;
                    fatal_down = true;
                    break 'down;
                }
                match drain_decrypted(&mut g.conn, &mut plain, &mut tls_plain) {
                    Ok(eof) => clean_eof = eof,
                    Err(e) => {
                        g.record_err(e);
                        g.done = true;
                        fatal_down = true;
                        break 'down;
                    }
                }
            }
            if g.conn.wants_write() {
                // Post-handshake housekeeping (e.g. a KeyUpdate reply) queued
                // outgoing TLS bytes: wake the writer thread.
                shared.cv.notify_all();
            }
        }
        if !plain.is_empty()
            && let Err(e) = local_down
                .write_all(&plain)
                .and_then(|()| local_down.flush())
        {
            let mut g = shared.lock();
            g.record_err(e);
            g.done = true;
            fatal_down = true;
            local_failed = true;
            break;
        }
        if clean_eof {
            break;
        }
    }
    if fatal_down {
        shared.lock().done = true;
    }
    shared.cv.notify_all();
    // Any end on the peer's side — a clean one, or a fatal one (a record that
    // does not decrypt, an alert, a write to the peer that failed, the cut) —
    // is told to the local end as an EOF after every plaintext byte above was
    // written and flushed, the line it is serving included, its own write
    // half left open. Hanging it up instead would wake the uploader at once
    // and let the guard go while that end still serves what it read, a
    // thread no guard counts. The uploader ends the relay once that end
    // answers (its first byte after a fatal end) or hangs up, and the peer
    // hears the end only from it, once the guard is gone. Only an end this
    // thread could not write to is hung up here.
    if local_failed {
        aterm_uds::hangup::hang_up(&local_down);
    } else {
        let _ = local_down.shutdown(std::net::Shutdown::Write);
    }
    let _ = up.join();
    {
        let mut g = shared.lock();
        g.done = true;
    }
    shared.cv.notify_all();
    aterm_uds::hangup::hang_up(&tcp_down);
    aterm_uds::hangup::hang_up(&local_down);
    let _ = wr.join();

    match shared.lock().err.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Write `bytes` to `tcp` as far as its send buffer takes them NOW, never
/// waiting on the peer ([`relay_answers`]' last word): a peer that has stopped
/// reading misses the rest, and nothing is reported, since the relay is over.
#[cfg(unix)]
fn send_without_waiting(tcp: &TcpStream, mut bytes: &[u8]) {
    use std::os::fd::AsRawFd as _;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const FLAGS: libc::c_int = libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    const FLAGS: libc::c_int = libc::MSG_DONTWAIT;
    while !bytes.is_empty() {
        // SAFETY: `tcp` keeps its descriptor open for the call, and `bytes`
        // is a live slice of `bytes.len()` readable bytes.
        let sent =
            unsafe { libc::send(tcp.as_raw_fd(), bytes.as_ptr().cast(), bytes.len(), FLAGS) };
        match usize::try_from(sent) {
            Ok(n) if n > 0 => bytes = &bytes[n.min(bytes.len())..],
            Err(_) if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
            _ => return,
        }
    }
}

/// No per-call non-blocking send here: the peer hears the FIN alone.
#[cfg(not(unix))]
fn send_without_waiting(_tcp: &TcpStream, _bytes: &[u8]) {}

/// A read with no data yet on a non-blocking/timeout socket (not a true block).
fn is_would_block(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// A peer closing the connection — expected during teardown, not a relay failure.
fn is_normal_close(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::PrivateKeyDer;
    use rustls::{ClientConfig, ClientConnection, ServerConfig, ServerConnection};
    use std::net::{TcpListener, TcpStream};

    const TEST_CERT_DER: &[u8] = include_bytes!("testdata/cert.der");
    const TEST_KEY_DER: &[u8] = include_bytes!("testdata/key.pkcs8.der");

    /// The tests' own exporter label: what the relay carries does not depend
    /// on it, only the exporters' agreement does.
    const EXPORTER_LABEL: &[u8] = b"EXPORTER-aterm-net-test";

    fn io_err(e: impl std::fmt::Display) -> io::Error {
        io::Error::other(e.to_string())
    }

    /// A TLS 1.3 listener configuration for the fixture's certificate and key.
    fn server_config(cert_der: Vec<u8>, key_pkcs8_der: Vec<u8>) -> io::Result<Arc<ServerConfig>> {
        init_crypto();
        let key = PrivateKeyDer::try_from(key_pkcs8_der).map_err(io_err)?;
        let cfg = ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(cert_der)], key)
            .map_err(io_err)?;
        Ok(Arc::new(cfg))
    }

    /// A TLS 1.3 dialer configuration pinning `fingerprint` through the
    /// production [`PinnedServerVerifier`].
    fn client_config(fingerprint: [u8; 32]) -> Arc<ClientConfig> {
        init_crypto();
        let cfg = ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedServerVerifier::new(fingerprint)))
            .with_no_client_auth();
        Arc::new(cfg)
    }

    /// The listener's handshake on an accepted `tcp`, exporting under
    /// [`EXPORTER_LABEL`].
    fn accept(
        tcp: TcpStream,
        config: Arc<ServerConfig>,
    ) -> io::Result<TlsTransport<ServerConnection>> {
        let mut conn = ServerConnection::new(config).map_err(io_err)?;
        let mut tcp = tcp;
        while conn.is_handshaking() {
            conn.complete_io(&mut tcp).map_err(listen_handshake_error)?;
        }
        let exporter = conn
            .export_keying_material([0u8; EXPORTER_LEN], EXPORTER_LABEL, None)
            .map_err(io_err)?;
        Ok(TlsTransport::from_parts(
            StreamOwned::new(conn, tcp),
            exporter,
        ))
    }

    /// The dialer's handshake on a connected `tcp`, exporting under
    /// [`EXPORTER_LABEL`].
    fn connect(
        tcp: TcpStream,
        server_name: ServerName<'static>,
        config: Arc<ClientConfig>,
    ) -> io::Result<TlsTransport<ClientConnection>> {
        let mut conn = ClientConnection::new(config, server_name).map_err(io_err)?;
        let mut tcp = tcp;
        while conn.is_handshaking() {
            conn.complete_io(&mut tcp).map_err(dial_handshake_error)?;
        }
        let exporter = conn
            .export_keying_material([0u8; EXPORTER_LEN], EXPORTER_LABEL, None)
            .map_err(io_err)?;
        Ok(TlsTransport::from_parts(
            StreamOwned::new(conn, tcp),
            exporter,
        ))
    }

    fn test_server_name() -> ServerName<'static> {
        ServerName::try_from("aterm-net-test").unwrap()
    }

    /// Serializes the loopback TLS tests in this module against EACH OTHER.
    ///
    /// Every one of them stands up a listener, a relay thread and a service
    /// thread, then waits on real socket I/O between them. Run concurrently —
    /// which is what the test harness does by default — the eight of them put
    /// ~24 threads into a scheduling fight over the same loopback path, and the
    /// relay threads lose: reads that take milliseconds when the test runs alone
    /// stall past a 60 s bound.
    ///
    /// Measured under ~950 % competing load: PARALLEL 2 failed after 60 s;
    /// SERIALIZED 22/22 in **1.56 s**. So the contention is between these tests,
    /// not with the rest of the machine — external load alone does not break
    /// them, and no timeout tuning fixes what is a thread-count problem.
    ///
    /// Poisoning is ignored on purpose (`into_inner`): a panic in one test must
    /// fail THAT test, not cascade into every later one as a poisoned-lock
    /// failure that hides the original cause.
    static LOOPBACK_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take [`LOOPBACK_TESTS`] for the rest of the calling test.
    fn serialize_loopback() -> std::sync::MutexGuard<'static, ()> {
        LOOPBACK_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn tls_handshake_exporters_match_and_a_wrong_pin_is_rejected() {
        let _serial = serialize_loopback();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();

        // Server thread: accept TLS, return the exporter.
        let srv = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let t = accept(tcp, scfg).unwrap();
            t.exporter().to_vec()
        });

        // Client: pin the REAL fingerprint -> handshake succeeds, exporter matches.
        let pin = cert_fingerprint(TEST_CERT_DER);
        let ccfg = client_config(pin);
        let tcp = TcpStream::connect(addr).unwrap();
        let ct = connect(tcp, test_server_name(), ccfg).unwrap();
        let client_exporter = ct.exporter().to_vec();

        let server_exporter = srv.join().unwrap();
        assert_eq!(
            client_exporter, server_exporter,
            "RFC 5705 exporter must be identical on both ends"
        );
        assert_eq!(client_exporter.len(), EXPORTER_LEN);
        assert_ne!(
            client_exporter, [0u8; EXPORTER_LEN],
            "exporter is real key material"
        );
    }

    #[test]
    fn a_pinned_tls_stream_relays_a_control_exchange_end_to_end() {
        let _serial = serialize_loopback();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let pin = cert_fingerprint(TEST_CERT_DER);

        // The "service" behind the listener's control socket: a byte echo. The
        // relay bridges the verified TLS peer to `svc_a`; `svc_b` echoes.
        let (svc_a, mut svc_b) = CtlStream::pair().unwrap();
        let echo = std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match svc_b.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if svc_b
                            .write_all(&buf[..n])
                            .and_then(|()| svc_b.flush())
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });

        // Listener: accept TLS, then relay.
        let srv = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let t = accept(tcp, scfg).unwrap();
            relay_answers(t, svc_a, ()).unwrap();
        });

        // Dialer: connect TLS (pinned), exchange, close.
        let ccfg = client_config(pin);
        let tcp = TcpStream::connect(addr).unwrap();
        let mut ct = connect(tcp, test_server_name(), ccfg).unwrap();

        // Drive a control round-trip THROUGH the relay (TLS -> svc_a -> echo -> back).
        ct.stream().write_all(b"ping\n").unwrap();
        ct.stream().flush().unwrap();
        let mut got = [0u8; 5];
        ct.stream().read_exact(&mut got).unwrap();
        assert_eq!(
            &got, b"ping\n",
            "the relay carried the control exchange round-trip"
        );

        // Clean close_notify -> the listener's relay sees a clean EOF and returns.
        {
            let s = ct.stream();
            s.conn.send_close_notify();
            let _ = s.flush();
        }
        drop(ct);
        srv.join().unwrap();
        let _ = echo.join();
    }

    #[test]
    fn relay_streams_a_large_payload_under_send_side_backpressure_without_truncating() {
        let _serial = serialize_loopback();
        // A payload larger than the kernel send buffers forces send-side
        // backpressure mid-stream: a transient WouldBlock must be retried from
        // the consumed offset, and the final drain must deliver the tail, so
        // every byte arrives in order.
        const PAYLOAD_LEN: usize = 1 << 20; // 1 MiB — well beyond any socket buffer
        let payload: Vec<u8> = (0..PAYLOAD_LEN).map(|i| (i % 251) as u8).collect();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let pin = cert_fingerprint(TEST_CERT_DER);

        // The "service" behind the control socket: write the whole payload, then
        // close so the relay's uploader sees a clean local EOF and drains.
        let (svc_a, mut svc_b) = CtlStream::pair().unwrap();
        let producer_payload = payload.clone();
        let producer = std::thread::spawn(move || {
            svc_b.write_all(&producer_payload).unwrap();
            svc_b.flush().unwrap();
            // svc_b drops here -> svc_a EOF.
        });

        // Listener: accept TLS, then relay svc_a <-> TLS.
        let srv = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let t = accept(tcp, scfg).unwrap();
            relay_answers(t, svc_a, ()).unwrap();
        });

        // Dialer: connect, then read the full payload back off the relayed TLS
        // stream and assert byte-for-byte fidelity.
        let ccfg = client_config(pin);
        let tcp = TcpStream::connect(addr).unwrap();
        let mut ct = connect(tcp, test_server_name(), ccfg).unwrap();

        let mut got = vec![0u8; PAYLOAD_LEN];
        ct.stream().read_exact(&mut got).unwrap();
        assert_eq!(
            got, payload,
            "the relay delivered every byte in order despite send-side backpressure \
             (a WouldBlock must retry, not truncate the stream)"
        );

        producer.join().unwrap();
        drop(ct);
        srv.join().unwrap();
    }

    /// A guard that says when it is dropped.
    #[cfg(unix)]
    struct SaysDropped(std::sync::mpsc::Sender<()>);

    #[cfg(unix)]
    impl Drop for SaysDropped {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    /// Whether a relay could write to its local end `side` now: `false` once
    /// the downloader has filled the local socket and is blocked on it.
    #[cfg(unix)]
    fn writable(side: &CtlStream) -> bool {
        use std::os::fd::AsRawFd as _;
        let mut probe = libc::pollfd {
            fd: side.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: one live `pollfd` for the call, which does not block.
        let ready = unsafe { libc::poll(&raw mut probe, 1, 0) };
        ready > 0 && probe.revents & libc::POLLOUT != 0
    }

    /// A guard whose drop says it has begun, then holds until the gate opens
    /// (its sender dropped): the hang detector, never a latency budget.
    struct HeldGuard {
        entered: std::sync::mpsc::Sender<()>,
        gate: std::sync::mpsc::Receiver<()>,
    }

    impl Drop for HeldGuard {
        fn drop(&mut self) {
            let _ = self.entered.send(());
            let _ = self.gate.recv_timeout(Duration::from_secs(60));
        }
    }

    /// [`answer`]'S ORDER, pinned as [`relay_answers`]' is: the answer is
    /// written and flushed, and nothing of the connection's end reaches the
    /// wire while the guard is held. The peer asks, reads its answer, waits
    /// for the guard's drop to begin and peeks its raw socket: nothing — no
    /// `close_notify`, no FIN. Only once the gate opens does the end arrive,
    /// a clean `close_notify`.
    ///
    /// NEGATIVE CONTROL (by mutation, run): with the guard dropped after the
    /// end, the peek finds the end on the wire. Every wait here is the hang
    /// detector, never a latency budget.
    #[test]
    fn an_answer_says_nothing_of_the_end_while_its_guard_is_held() {
        use std::sync::mpsc;
        let _serial = serialize_loopback();
        const HANG: Duration = Duration::from_secs(60);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let pin = cert_fingerprint(TEST_CERT_DER);
        let (entered_tx, entered) = mpsc::channel();
        let (open, gate) = mpsc::channel::<()>();
        let srv = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            tcp.set_read_timeout(Some(HANG)).unwrap();
            let t = accept(tcp, scfg).unwrap();
            let guard = HeldGuard {
                entered: entered_tx,
                gate,
            };
            answer(t, guard, |stream| {
                let mut asked = [0u8; 5];
                stream.read_exact(&mut asked)?;
                stream.write_all(b"pong\n")
            })
        });
        let tcp = TcpStream::connect(addr).unwrap();
        let mut ct = connect(tcp, test_server_name(), client_config(pin)).unwrap();
        ct.stream().get_ref().set_read_timeout(Some(HANG)).unwrap();
        ct.stream().write_all(b"ping\n").unwrap();
        ct.stream().flush().unwrap();
        let mut got = [0u8; 5];
        ct.stream().read_exact(&mut got).unwrap();
        assert_eq!(&got, b"pong\n");
        entered
            .recv_timeout(HANG)
            .expect("the guard is dropped once the answer is written");
        let raw = ct.stream().get_ref();
        raw.set_nonblocking(true).unwrap();
        let mut probe = [0u8; 64];
        let peeked = raw.peek(&mut probe);
        assert!(
            matches!(&peeked, Err(e) if e.kind() == io::ErrorKind::WouldBlock),
            "nothing of the end while the guard is held: {peeked:?}"
        );
        raw.set_nonblocking(false).unwrap();
        drop(open);
        let mut rest = Vec::new();
        let end = ct.stream().read_to_end(&mut rest);
        assert!(
            end.is_ok() && rest.is_empty(),
            "a clean close_notify ends it: {end:?}, {} more bytes",
            rest.len()
        );
        assert!(matches!(srv.join().unwrap(), Ok(())));
    }

    /// [`relay_answers`] ordering: nothing of the connection's end reaches the
    /// wire while the guard is held. Only once the gate opens does the end
    /// arrive (`close_notify`), and the relay returns while the peer holds its
    /// end open and silent.
    #[test]
    fn an_answering_relay_says_nothing_of_the_end_while_its_guard_is_held() {
        use std::sync::mpsc;
        let _serial = serialize_loopback();
        const PAYLOAD_LEN: usize = 1 << 20;
        const HANG: Duration = Duration::from_secs(60);
        let payload: Vec<u8> = (0..PAYLOAD_LEN).map(|i| (i % 251) as u8).collect();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let pin = cert_fingerprint(TEST_CERT_DER);
        let (entered_tx, entered) = mpsc::channel();
        let (open, gate) = mpsc::channel::<()>();

        let (svc_a, mut svc_b) = CtlStream::pair().unwrap();
        let producer_payload = payload.clone();
        let producer = std::thread::spawn(move || {
            svc_b.write_all(&producer_payload).unwrap();
            svc_b.flush().unwrap();
            aterm_uds::hangup::hang_up(&svc_b);
        });
        let (relayed_tx, relayed) = mpsc::channel();
        let srv = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let t = accept(tcp, scfg).unwrap();
            let guard = HeldGuard {
                entered: entered_tx,
                gate,
            };
            let _ = relayed_tx.send(relay_answers(t, svc_a, guard));
        });

        let tcp = TcpStream::connect(addr).unwrap();
        let mut ct = connect(tcp, test_server_name(), client_config(pin)).unwrap();
        ct.stream().get_ref().set_read_timeout(Some(HANG)).unwrap();
        let mut got = vec![0u8; PAYLOAD_LEN];
        ct.stream().read_exact(&mut got).unwrap();
        assert!(got == payload, "every answer byte, in order");
        entered
            .recv_timeout(HANG)
            .expect("the guard is dropped once the answer is over");
        // The guard is held: nothing of the end may be on the wire.
        let raw = ct.stream().get_ref();
        raw.set_nonblocking(true).unwrap();
        let mut probe = [0u8; 64];
        let peeked = raw.peek(&mut probe);
        assert!(
            matches!(&peeked, Err(e) if e.kind() == io::ErrorKind::WouldBlock),
            "nothing of the end while the guard is held: {peeked:?}"
        );
        raw.set_nonblocking(false).unwrap();
        // The gate opens: the end arrives, a clean close_notify.
        drop(open);
        let mut rest = Vec::new();
        let end = ct.stream().read_to_end(&mut rest);
        assert!(
            end.is_ok() && rest.is_empty(),
            "a clean close_notify ends it: {end:?}, {} more bytes",
            rest.len()
        );
        // The peer still holds its end, silent: the relay is over regardless.
        let over = relayed.recv_timeout(HANG);
        assert!(matches!(over, Ok(Ok(()))), "{over:?}");
        srv.join().unwrap();
        producer.join().unwrap();
        drop(ct);
    }

    /// [`relay_answers`] lets its guard go only once neither end can hold it:
    /// when a connection with blocked downloader and uploader is cut, the relay
    /// returns with its guard dropped and the local end's write fails.
    #[cfg(unix)]
    #[test]
    fn a_cut_answering_relay_ends_though_neither_end_reads() {
        use std::sync::mpsc;
        let _serial = serialize_loopback();
        const HANG: Duration = Duration::from_secs(60);
        const PIPELINED: usize = 16 << 20;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let pin = cert_fingerprint(TEST_CERT_DER);

        let (svc_a, mut svc_b) = CtlStream::pair().unwrap();
        let relay_side = svc_a.try_clone().unwrap();
        // The local end: an answer without end, never a read.
        let (answered_tx, answered) = mpsc::channel();
        let answering = std::thread::spawn(move || {
            let chunk = vec![b'a'; 64 * 1024];
            let failed = loop {
                if let Err(e) = svc_b.write_all(&chunk) {
                    break e;
                }
            };
            let _ = answered_tx.send(failed.kind());
        });
        let (dropped_tx, dropped) = mpsc::channel();
        let (line_tx, line) = mpsc::channel();
        let (relayed_tx, relayed) = mpsc::channel();
        let srv = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            line_tx.send(tcp.try_clone().unwrap()).unwrap();
            let t = accept(tcp, scfg).unwrap();
            let _ = relayed_tx.send(relay_answers(t, svc_a, SaysDropped(dropped_tx)));
        });

        let tcp = TcpStream::connect(addr).unwrap();
        let mut ct = connect(tcp, test_server_name(), client_config(pin)).unwrap();
        // The peer: a pipeline the local end never reads, and no read.
        let (piped_tx, piped) = mpsc::channel();
        let pipelining = std::thread::spawn(move || {
            let _ = piped_tx.send(ct.stream().write_all(&vec![b'p'; PIPELINED]));
        });
        let line = line.recv_timeout(HANG).expect("the server accepts");
        // The downloader has filled the local socket and is blocked on it.
        let filled = Instant::now() + HANG;
        while writable(&relay_side) {
            assert!(
                Instant::now() < filled,
                "the relay never filled its local end"
            );
            std::thread::yield_now();
        }
        // The cut, as `fleet::LiveConnections` makes it.
        assert!(aterm_uds::hangup::hang_up(&line), "the cut shuts the line");
        let over = relayed.recv_timeout(HANG);
        assert!(over.is_ok(), "the cut relay returns: {over:?}");
        assert!(dropped.try_recv().is_ok(), "its guard is gone");
        // The relay's last descriptor of the local socket closes with this
        // probe's: the local end's write then fails.
        drop(relay_side);
        let failed = answered.recv_timeout(HANG);
        assert!(failed.is_ok(), "the local end's write fails: {failed:?}");
        drop(line);
        srv.join().unwrap();
        answering.join().unwrap();
        // The peer's pipeline ends too, its socket closed under it.
        let piped = piped.recv_timeout(HANG);
        assert!(piped.is_ok(), "the peer's writes end: {piped:?}");
        pipelining.join().unwrap();
    }

    /// [`relay_answers`] hangs its local end up one half at a time: when the
    /// local end answers and hangs up while a cloned descriptor of it remains
    /// open and the downloader is blocked writing to it, the hang-up wakes the
    /// downloader and the relay returns with its guard dropped.
    #[cfg(unix)]
    #[test]
    fn an_answering_relay_ends_though_a_copy_of_its_hung_up_local_end_lives_on() {
        use std::sync::mpsc;
        let _serial = serialize_loopback();
        const HANG: Duration = Duration::from_secs(60);
        const PIPELINED: usize = 16 << 20;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let pin = cert_fingerprint(TEST_CERT_DER);

        let (svc_a, mut svc_b) = CtlStream::pair().unwrap();
        let relay_side = svc_a.try_clone().unwrap();
        let lingering = svc_b.try_clone().unwrap();
        let (dropped_tx, dropped) = mpsc::channel();
        let (relayed_tx, relayed) = mpsc::channel();
        let srv = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let t = accept(tcp, scfg).unwrap();
            let _ = relayed_tx.send(relay_answers(t, svc_a, SaysDropped(dropped_tx)));
        });

        let tcp = TcpStream::connect(addr).unwrap();
        let mut ct = connect(tcp, test_server_name(), client_config(pin)).unwrap();
        // The peer: a pipeline the local end never reads, and no read.
        let (piped_tx, piped) = mpsc::channel();
        let pipelining = std::thread::spawn(move || {
            let _ = piped_tx.send(ct.stream().write_all(&vec![b'p'; PIPELINED]));
        });
        // The downloader has filled the local socket and is blocked on it.
        let filled = Instant::now() + HANG;
        while writable(&relay_side) {
            assert!(
                Instant::now() < filled,
                "the relay never filled its local end"
            );
            std::thread::yield_now();
        }
        drop(relay_side);
        // The local end answers and hangs up; its copy lives on.
        svc_b.write_all(b"answered\n").unwrap();
        svc_b.flush().unwrap();
        aterm_uds::hangup::hang_up(&svc_b);
        let over = relayed.recv_timeout(HANG);
        assert!(
            over.is_ok(),
            "the relay returns while a copy of its local end is open: {over:?}"
        );
        assert!(dropped.try_recv().is_ok(), "its guard is gone");
        drop(lingering);
        drop(svc_b);
        srv.join().unwrap();
        // The peer's pipeline ends too, its socket closed under it.
        let piped = piped.recv_timeout(HANG);
        assert!(piped.is_ok(), "the peer's writes end: {piped:?}");
        pipelining.join().unwrap();
    }

    /// A guard that says, as it is dropped, whether `answered` was set by then.
    #[cfg(unix)]
    struct SaysAnswered {
        answered: Arc<std::sync::atomic::AtomicBool>,
        said: std::sync::mpsc::Sender<bool>,
    }

    #[cfg(unix)]
    impl Drop for SaysAnswered {
        fn drop(&mut self) {
            let answered = self.answered.load(std::sync::atomic::Ordering::SeqCst);
            let _ = self.said.send(answered);
        }
    }

    /// [`relay_answers`] holds its guard through the line its local end serves
    /// even when the peer sends an undecryptable record after the request: the
    /// local end reads EOF after its line and only its answer releases the
    /// guard.
    #[cfg(unix)]
    #[test]
    fn an_answering_relay_whose_peer_fails_holds_its_guard_through_the_line() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;
        let _serial = serialize_loopback();
        const HANG: Duration = Duration::from_secs(60);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let pin = cert_fingerprint(TEST_CERT_DER);

        let (svc_a, mut svc_b) = CtlStream::pair().unwrap();
        svc_b.set_read_timeout(Some(HANG)).unwrap();
        let answered = Arc::new(AtomicBool::new(false));
        let (said_tx, said) = mpsc::channel();
        let guard = SaysAnswered {
            answered: Arc::clone(&answered),
            said: said_tx,
        };
        let (relayed_tx, relayed) = mpsc::channel();
        let srv = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let t = accept(tcp, scfg).unwrap();
            let _ = relayed_tx.send(relay_answers(t, svc_a, guard));
        });
        // The local end: one line read, then whatever the relay says next;
        // then its answer, and its hang-up.
        let (asked_tx, asked) = mpsc::channel();
        let serving = std::thread::spawn(move || {
            let mut line = Vec::new();
            let mut byte = [0u8; 1];
            while svc_b.read(&mut byte)? == 1 && byte[0] != b'\n' {
                line.push(byte[0]);
            }
            let _ = asked_tx.send(line);
            let next = svc_b.read(&mut byte)?;
            let still_read = writable(&svc_b);
            answered.store(true, Ordering::SeqCst);
            svc_b.write_all(b"OK 0\n")?;
            svc_b.flush()?;
            aterm_uds::hangup::hang_up(&svc_b);
            Ok::<_, io::Error>((next, still_read))
        });

        let tcp = TcpStream::connect(addr).unwrap();
        let mut ct = connect(tcp, test_server_name(), client_config(pin)).unwrap();
        ct.stream().write_all(b"survey\n").unwrap();
        ct.stream().flush().unwrap();
        let line = asked.recv_timeout(HANG);
        assert!(
            matches!(&line, Ok(line) if line == b"survey"),
            "the local end reads the request: {line:?}"
        );
        // An application-data record no key opens.
        let mut record = vec![0x17, 0x03, 0x03, 0x00, 0x20];
        record.extend_from_slice(&[0xaa; 0x20]);
        let mut raw = ct.stream().get_ref();
        raw.write_all(&record).unwrap();
        raw.flush().unwrap();

        let served = serving.join().unwrap();
        assert!(
            matches!(served, Ok((0, true))),
            "the local end reads an EOF after its line, and the relay still reads it: {served:?}"
        );
        let first = said.recv_timeout(HANG);
        assert!(
            matches!(first, Ok(true)),
            "the guard goes only once the local end has answered: {first:?}"
        );
        let over = relayed.recv_timeout(HANG);
        assert!(
            matches!(&over, Ok(Err(_))),
            "the relay returns the peer's failure: {over:?}"
        );
        srv.join().unwrap();
        drop(ct);
    }

    #[test]
    fn relay_early_request_close_delivers_guarded_reply_but_not_late_ack() {
        let _serial = serialize_loopback();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let pin = cert_fingerprint(TEST_CERT_DER);
        let (svc_a, mut svc_b) = CtlStream::pair().unwrap();
        // SEQUENCED: unsequenced, the request and the client's close_notify race through
        // the relay and can wedge the test. The service `read_exact`s the request
        // and says so on a channel; the client sends its close_notify only after
        // that signal; the service then reads the EOF, and only then writes the
        // response.
        svc_b
            .set_read_timeout(Some(NET_TEST_LIVENESS_BOUND))
            .unwrap();
        let mut response = (0..(128 * 1024))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        const NONCE: &[u8] = b"00112233445566778899aabbccddeeff";
        response.extend_from_slice(b"\nACK-CHALLENGE ");
        response.extend_from_slice(NONCE);
        response.push(b'\n');
        let expected_response = response.clone();
        let (ack_attempted_tx, ack_attempted_rx) = std::sync::mpsc::channel();

        let (request_seen_tx, request_seen_rx) = std::sync::mpsc::channel();
        let service = std::thread::spawn(move || {
            const REQUEST: &[u8] = b"one-shot request\n";
            let mut request = vec![0u8; REQUEST.len()];
            svc_b
                .read_exact(&mut request)
                .expect("the relay must deliver the request within the read timeout");
            assert_eq!(request, REQUEST);
            request_seen_tx.send(()).unwrap();
            // The request direction's EOF: the client's close_notify, relayed.
            let mut rest = Vec::new();
            svc_b.read_to_end(&mut rest).unwrap();
            assert!(rest.is_empty(), "nothing follows the one-shot request");
            svc_b.write_all(&response).unwrap();
            svc_b.flush().unwrap();
            ack_attempted_rx
                .recv_timeout(NET_TEST_LIVENESS_BOUND)
                .unwrap();
            let mut reverse = Vec::new();
            svc_b.read_to_end(&mut reverse).unwrap();
            assert!(
                reverse.is_empty(),
                "an ACK written after the downstream close_notify must not reach upstream"
            );
            svc_b.shutdown(std::net::Shutdown::Write).unwrap();
        });
        let server = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let transport = accept(tcp, scfg).unwrap();
            relay_answers(transport, svc_a, ()).unwrap();
        });

        let tcp = TcpStream::connect(addr).unwrap();
        // The client's half of the same bound (see the service side above).
        tcp.set_read_timeout(Some(NET_TEST_LIVENESS_BOUND)).unwrap();
        let mut client = connect(tcp, test_server_name(), client_config(pin)).unwrap();
        client.stream().write_all(b"one-shot request\n").unwrap();
        client.stream().flush().unwrap();
        // Sequenced: the service holds the whole request before the close_notify.
        request_seen_rx
            .recv_timeout(NET_TEST_LIVENESS_BOUND)
            .expect("the service should announce the request");
        // Directional request EOF must not tear down the still-live response
        // direction. The peer service writes only after observing this EOF.
        client.stream().conn.send_close_notify();
        client.stream().flush().unwrap();
        client
            .stream()
            .sock
            .shutdown(std::net::Shutdown::Write)
            .unwrap();

        let mut got = vec![0; expected_response.len()];
        client.stream().read_exact(&mut got).unwrap_or_else(|error| {
            panic!(
                "the client's read_exact stalled past {NET_TEST_LIVENESS_BOUND:?} ({error}): the \
                 relay never delivered the whole response after a request-first half-close. \
                 The measured shape of this stall: the client in FIN_WAIT_2, the relay's three \
                 clones of the accepted socket (tcp_down / tcp_wr / tcp_up) in CLOSE_WAIT, \
                 svc_a half-closed for writing by the downloader, and the downloader parked on \
                 `up.join()` because the reverse direction never reached EOF — the shape the \
                 request-before-close_notify sequencing above exists to rule out"
            )
        });
        assert_eq!(
            got, expected_response,
            "every response byte must survive request-first half-close"
        );
        let mut late_ack = b"ACK ".to_vec();
        late_ack.extend_from_slice(NONCE);
        late_ack.push(b'\n');
        let _ = client.stream().write_all(&late_ack);
        let _ = client.stream().flush();
        ack_attempted_tx.send(()).unwrap();

        service.join().unwrap();
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn relay_round_trips_guarded_artifact_ack_before_request_half_close() {
        let _serial = serialize_loopback();
        const REQUEST: &[u8] = b"image guarded-demo.png\n";
        const BODY: &[u8] = b"OK /tmp/aterm/images/guarded-demo.png\n";
        const NONCE: &[u8] = b"fedcba98765432100123456789abcdef";

        let mut guarded_reply = BODY.to_vec();
        guarded_reply.extend_from_slice(b"ACK-CHALLENGE ");
        guarded_reply.extend_from_slice(NONCE);
        guarded_reply.push(b'\n');
        let expected_reply = guarded_reply.clone();

        let mut expected_ack = b"ACK ".to_vec();
        expected_ack.extend_from_slice(NONCE);
        expected_ack.push(b'\n');
        let client_ack = expected_ack.clone();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let pin = cert_fingerprint(TEST_CERT_DER);
        let (svc_a, mut svc_b) = CtlStream::pair().unwrap();
        svc_b
            .set_read_timeout(Some(NET_TEST_LIVENESS_BOUND))
            .unwrap();
        let (server_ready_tx, server_ready_rx) = std::sync::mpsc::channel();
        let (request_flushed_tx, request_flushed_rx) = std::sync::mpsc::channel();

        let service = std::thread::spawn(move || {
            let mut request = vec![0; REQUEST.len()];
            svc_b.read_exact(&mut request).unwrap();
            assert_eq!(request, REQUEST);

            svc_b.write_all(&guarded_reply).unwrap();
            svc_b.flush().unwrap();

            let mut ack = vec![0; expected_ack.len()];
            svc_b.read_exact(&mut ack).unwrap();
            assert_eq!(
                ack, expected_ack,
                "the exact nonce ACK must reach the guarded upstream reply"
            );
            svc_b.shutdown(std::net::Shutdown::Write).unwrap();
        });
        let server = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            tcp.set_read_timeout(Some(NET_TEST_LIVENESS_BOUND)).unwrap();
            let mut transport = accept(tcp, scfg).unwrap();
            server_ready_tx.send(()).unwrap();
            request_flushed_rx
                .recv_timeout(NET_TEST_LIVENESS_BOUND)
                .unwrap();

            // Deterministically recreate the production race: consume the first
            // application record into rustls before `relay_answers` starts. This is the
            // same state `accept` reaches when the peer's Finished and request
            // share a TCP read. The relay must drain this buffered plaintext
            // before blocking for another network record.
            let stream = transport.stream();
            loop {
                let consumed = stream.conn.read_tls(&mut stream.sock).unwrap();
                assert!(consumed > 0, "the request must seed rustls' input buffer");
                let io = stream.conn.process_new_packets().unwrap();
                if io.plaintext_bytes_to_read() >= REQUEST.len() {
                    break;
                }
            }

            // Tier-1 guard bind: the real transport is now in the model's
            // initial `buffered=1, relayed=0, service_waiting=1` state. The
            // shipping relay below must take `DrainBuffered`; the Buggy=1
            // negative control demands a fresh read and is disabled here.
            let model = aterm_models_session::tls_buffered_relay_model();
            let state = model.init_state();
            assert!(model.action_enabled("DrainBuffered", &state));
            let mut buggy = model.clone();
            buggy
                .consts
                .iter_mut()
                .find(|(name, _)| *name == "Buggy")
                .unwrap()
                .1 = 1;
            assert!(!buggy.action_enabled("DrainBuffered", &state));

            relay_answers(transport, svc_a, ()).unwrap();
        });

        let tcp = TcpStream::connect(addr).unwrap();
        tcp.set_read_timeout(Some(NET_TEST_LIVENESS_BOUND)).unwrap();
        let mut client = connect(tcp, test_server_name(), client_config(pin)).unwrap();
        server_ready_rx
            .recv_timeout(NET_TEST_LIVENESS_BOUND)
            .unwrap();
        client.stream().write_all(REQUEST).unwrap();
        client.stream().flush().unwrap();
        request_flushed_tx.send(()).unwrap();

        let mut got = vec![0; expected_reply.len()];
        client.stream().read_exact(&mut got).unwrap();
        assert_eq!(
            got, expected_reply,
            "ordinary body and challenge trailer must arrive together and intact"
        );

        client.stream().write_all(&client_ack).unwrap();
        client.stream().flush().unwrap();
        // A one-shot client may half-close only after its causal ACK is on the
        // wire; TLS close_notify cannot be followed by more application data.
        client.stream().conn.send_close_notify();
        client.stream().flush().unwrap();
        client
            .stream()
            .sock
            .shutdown(std::net::Shutdown::Write)
            .unwrap();

        let mut tail = Vec::new();
        client.stream().read_to_end(&mut tail).unwrap();
        assert!(tail.is_empty());
        service.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn a_wrong_fingerprint_pin_rejects_the_handshake() {
        let _serial = serialize_loopback();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let srv = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().ok()?;
            // Expected to fail: the client refuses the certificate.
            accept(tcp, scfg).err()
        });
        // Pin a DIFFERENT fingerprint -> the client must refuse the server cert.
        let mut wrong = cert_fingerprint(TEST_CERT_DER);
        wrong[0] ^= 0xff;
        let ccfg = client_config(wrong);
        let tcp = TcpStream::connect(addr).unwrap();
        let Err(err) = connect(tcp, test_server_name(), ccfg) else {
            panic!("a mismatched fingerprint pin must reject the handshake");
        };
        // The dialer says both digests, in the spelling `fingerprint` takes.
        let said = err.to_string();
        assert!(said.starts_with("certificate mismatch: "), "{said}");
        assert!(
            said.contains(&format!(
                "presents sha256:{}",
                aterm_codec::hex::encode_lower(&cert_fingerprint(TEST_CERT_DER))
            )),
            "{said}"
        );
        assert!(
            said.contains(&format!(
                "fingerprint is sha256:{}",
                aterm_codec::hex::encode_lower(&wrong)
            )),
            "{said}"
        );
        // The listener says what the dialer did, not a bare rustls alert name.
        let heard = srv.join().unwrap().expect("the listener's handshake fails");
        assert!(
            heard
                .to_string()
                .contains("refused this listener's certificate"),
            "{heard}"
        );
    }

    /// Upper bound on how long a socket read in these tests may block.
    ///
    /// Not a latency budget — a liveness guard. Its only job is that a stall can
    /// never be UNBOUNDED, because an unbounded one hangs `targo --unverified
    /// test --workspace` (and therefore `verify test`) instead of failing it.
    /// Sized to survive worst-case scheduler starvation when the whole
    /// workspace suite runs concurrently, not to measure a loopback round trip,
    /// which takes milliseconds.
    const NET_TEST_LIVENESS_BOUND: Duration = Duration::from_secs(60);

    /// The download direction drains rustls' 16 KiB plaintext buffer after each
    /// packet batch so a socket read carrying a full record's tail followed by
    /// small records never overflows the buffer mid-feed.
    #[test]
    fn relay_ingests_a_full_record_followed_by_small_ones_without_tearing_down() {
        let _serial = serialize_loopback();
        const FULL: usize = 16 * 1024;
        const SMALL: usize = 100;
        const SMALLS: usize = 200;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let pin = cert_fingerprint(TEST_CERT_DER);
        let mut expected = (0..FULL).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        for record in 0..SMALLS {
            expected.extend((0..SMALL).map(|i| ((record + i) % 241) as u8));
        }
        let to_send = expected.clone();
        let (written_tx, written_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let peer = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let mut transport = accept(tcp, scfg).unwrap();
            let stream = transport.stream();
            // One `write` per record: a full one, then the small ones.
            stream.write_all(&to_send[..FULL]).unwrap();
            for record in to_send[FULL..].chunks(SMALL) {
                stream.write_all(record).unwrap();
            }
            stream.flush().unwrap();
            written_tx.send(()).unwrap();
            // Hold the connection open until the dialer has its bytes, then
            // end it cleanly.
            let _ = release_rx.recv_timeout(NET_TEST_LIVENESS_BOUND);
            stream.conn.send_close_notify();
            let _ = stream.flush();
        });

        let tcp = TcpStream::connect(addr).unwrap();
        let transport = connect(tcp, test_server_name(), client_config(pin)).unwrap();
        written_rx.recv_timeout(NET_TEST_LIVENESS_BOUND).unwrap();
        // Every record's bytes are on the wire; wait until they are all in this
        // socket's receive buffer (a must-happen wait, bounded as a hang
        // detector), so the relay's reads see them at fixed offsets.
        let wire_at_least = FULL + SMALLS * SMALL;
        let started = Instant::now();
        let mut peek = vec![0u8; 4 * wire_at_least];
        while transport.stream.sock.peek(&mut peek).unwrap() < wire_at_least {
            assert!(
                started.elapsed() < NET_TEST_LIVENESS_BOUND,
                "the listener's records never arrived"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        let (local_relay, mut local_app) = CtlStream::pair().unwrap();
        let relayed = std::thread::spawn(move || relay_answers(transport, local_relay, ()));
        local_app
            .set_read_timeout(Some(NET_TEST_LIVENESS_BOUND))
            .unwrap();
        let mut got = Vec::with_capacity(expected.len());
        let mut buf = [0u8; 4096];
        while got.len() < expected.len() {
            match local_app.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => got.extend_from_slice(&buf[..n]),
            }
        }
        release_tx.send(()).unwrap();
        // The relay may already have shut this pair down (it does on a fatal
        // error), so the result is not the point.
        aterm_uds::hangup::hang_up(&local_app);
        let outcome = relayed.join().unwrap();
        peer.join().unwrap();
        assert!(
            outcome.is_ok(),
            "the relay tore down a healthy peer's stream: {outcome:?}"
        );
        assert_eq!(
            got.len(),
            expected.len(),
            "the relay delivered {} of {} plaintext bytes",
            got.len(),
            expected.len()
        );
        assert_eq!(got, expected);
    }
}
