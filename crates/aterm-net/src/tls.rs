// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TLS 1.3 transport (rustls + the `ring` provider) for the fleet ([`crate::fleet`]).

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use aterm_uds::CtlStream;

/// Cancellation of one connection preparation, before any admission proof is sent.
/// The stop interrupts connect/broker waits; the current phase uses its existing
/// socket watchdog, which is joined before that socket closes.
#[derive(Default)]
pub struct PreparationControl {
    stop: aterm_uds::stop::Stop,
    phase: Mutex<Option<Arc<crate::slots::Evictor>>>,
}

impl PreparationControl {
    /// A preparation which has not been cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same latched cancellation used by native connect and broker waits.
    #[must_use]
    #[cfg(unix)]
    pub(crate) fn stop(&self) -> &aterm_uds::stop::Stop {
        &self.stop
    }

    /// Cancel this preparation and interrupt its current socket I/O, if any.
    pub fn cancel(&self) {
        self.stop.set();
        if let Some(phase) = self
            .phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            phase.evict();
        }
    }

    pub(crate) fn check(&self) -> io::Result<()> {
        if self.stop.is_set() {
            Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "connection preparation was cancelled",
            ))
        } else {
            Ok(())
        }
    }

    pub(crate) fn register(
        self: &Arc<Self>,
        watchdog: &crate::slots::HandshakeWatchdog,
    ) -> io::Result<PreparationPhase> {
        let phase = Arc::new(watchdog.evictor());
        let mut current = self
            .phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.is_some() {
            return Err(io::Error::other(
                "a connection preparation already owns a socket phase",
            ));
        }
        *current = Some(Arc::clone(&phase));
        if self.stop.is_set() {
            phase.evict();
        }
        Ok(PreparationPhase {
            control: Arc::clone(self),
            phase,
        })
    }
}

pub(crate) struct PreparationPhase {
    control: Arc<PreparationControl>,
    phase: Arc<crate::slots::Evictor>,
}

impl Drop for PreparationPhase {
    fn drop(&mut self) {
        let mut current = self
            .control
            .phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current
            .as_ref()
            .is_some_and(|phase| Arc::ptr_eq(phase, &self.phase))
        {
            *current = None;
        }
    }
}

use aterm_digest::Sha256;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme, StreamOwned};

/// The exporter length: what every proof over a channel is taken over.
pub const EXPORTER_LEN: usize = 32;

fn provider() -> rustls::crypto::CryptoProvider {
    rustls::crypto::ring::default_provider()
}

/// Install `provider()` as the process default, once.
pub fn init_crypto() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
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

/// Verifies the server certificate by SHA-256 fingerprint pinning and provider signatures.
#[derive(Debug)]
pub(crate) struct PinnedServerVerifier {
    pin: [u8; 32],
    supported: WebPkiSupportedAlgorithms,
}

impl PinnedServerVerifier {
    /// Create a verifier accepting the certificate with SHA-256 fingerprint `pin`.
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
        let presented = cert_fingerprint(end_entity.as_ref());
        if presented == self.pin {
            Ok(ServerCertVerified::assertion())
        } else {
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
/// and what the connection pins.
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

/// A dialer's handshake error.
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

/// A listener's handshake error: a dialer's refusal of this listener's certificate in its own
/// words, every other error as rustls gave it.
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

/// A connected TLS stream plus its channel exporter — what a fleet proof is taken over.
pub struct TlsTransport<C> {
    stream: StreamOwned<C, TcpStream>,
    exporter: [u8; EXPORTER_LEN],
}

impl<C> TlsTransport<C> {
    /// Wrap a completed TLS `stream` with its caller-exported keying material.
    #[inline]
    pub(crate) const fn from_parts(
        stream: StreamOwned<C, TcpStream>,
        exporter: [u8; EXPORTER_LEN],
    ) -> Self {
        Self { stream, exporter }
    }

    /// The RFC 5705 keying material this connection's owner exported.
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

    /// Consume into a stream whose entire remaining exchange ends at `until`.
    ///
    /// Capture `until` before dialing to include connection and handshake time in the same
    /// budget. Progress never extends it: a watchdog interrupts blocked socket I/O at the
    /// deadline, and subsequent reads and writes return [`io::ErrorKind::TimedOut`].
    ///
    /// # Errors
    /// If the socket watchdog cannot start.
    pub fn into_bounded_stream(self, until: Instant) -> io::Result<BoundedStream<C>> {
        let watchdog = crate::slots::HandshakeWatchdog::arm_until(&self.stream.sock, until)?;
        Ok(BoundedStream {
            preparation: None,
            watchdog,
            stream: self.stream,
            until,
        })
    }

    /// Bound the exchange as above while its read-only preparation remains cancellable.
    /// The control must not also own another socket phase.
    ///
    /// # Errors
    /// Cancellation, an occupied control, or failure to arm the socket watchdog.
    pub fn into_preparing_stream(
        self,
        until: Instant,
        control: Arc<PreparationControl>,
    ) -> io::Result<BoundedStream<C>> {
        control.check()?;
        let watchdog = crate::slots::HandshakeWatchdog::arm_until(&self.stream.sock, until)?;
        let preparation = control.register(&watchdog)?;
        Ok(BoundedStream {
            preparation: Some(preparation),
            watchdog,
            stream: self.stream,
            until,
        })
    }

    /// The socket under the stream.
    #[inline]
    pub(crate) const fn tcp(&self) -> &TcpStream {
        &self.stream.sock
    }
}

/// An owned TLS exchange with one absolute deadline for all reads and writes.
///
/// Dropping this stream cancels and joins its watchdog before closing the socket. The raw TLS
/// stream is deliberately retained here so no socket can escape the deadline's ownership.
pub struct BoundedStream<C> {
    preparation: Option<PreparationPhase>,
    // Fields drop in declaration order. This guard must join before `stream` closes the socket
    // whose descriptor the Unix watchdog borrows.
    watchdog: crate::slots::HandshakeWatchdog,
    stream: StreamOwned<C, TcpStream>,
    until: Instant,
}

impl<C> BoundedStream<C> {
    fn expired(&self) -> bool {
        Instant::now() >= self.until || self.watchdog.fired()
    }

    fn check(&self) -> io::Result<()> {
        if let Some(preparation) = &self.preparation {
            preparation.control.check()?;
        }
        if self.expired() {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "TLS exchange deadline elapsed",
            ))
        } else {
            Ok(())
        }
    }
}

impl<C, S> Read for BoundedStream<C>
where
    C: std::ops::DerefMut + std::ops::Deref<Target = rustls::ConnectionCommon<S>>,
    S: rustls::SideData,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        let result = self.stream.read(buf);
        self.check()?;
        result
    }
}

impl<C, S> Write for BoundedStream<C>
where
    C: std::ops::DerefMut + std::ops::Deref<Target = rustls::ConnectionCommon<S>>,
    S: rustls::SideData,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.check()?;
        let result = self.stream.write(buf);
        self.check()?;
        result
    }

    fn flush(&mut self) -> io::Result<()> {
        self.check()?;
        let result = self.stream.flush();
        self.check()?;
        result
    }
}

/// Stream on which an [`answer`] is written, exposing I/O, timeouts, and peer address.
pub struct Answering<'a, C> {
    stream: &'a mut StreamOwned<C, TcpStream>,
    authorize: &'a (dyn Fn() -> io::Result<()> + Sync),
}

impl<'a, C> Answering<'a, C> {
    /// Recheck this connection's admission at a persistence boundary after reading its request.
    pub fn authorization(&self) -> &'a (dyn Fn() -> io::Result<()> + Sync) {
        self.authorize
    }
    /// Bound each read of the request ([`TcpStream::set_read_timeout`]).
    /// # Errors
    /// The socket's own.
    #[inline]
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.sock.set_read_timeout(timeout)
    }

    /// Bound each write of the answer ([`TcpStream::set_write_timeout`]).
    /// # Errors
    /// The socket's own.
    #[inline]
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.sock.set_write_timeout(timeout)
    }

    /// The address the peer dialled from.
    /// # Errors
    /// The socket's own.
    #[inline]
    pub fn peer_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.stream.sock.peer_addr()
    }

    /// The socket under the stream, for a test listener that writes raw records past TLS.
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
        (self.authorize)()?;
        self.stream.read(buf)
    }
}

impl<C, S> Write for Answering<'_, C>
where
    C: std::ops::DerefMut + std::ops::Deref<Target = rustls::ConnectionCommon<S>>,
    S: rustls::SideData,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (self.authorize)()?;
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        (self.authorize)()?;
        self.stream.flush()
    }
}

/// Answer `transport` HERE, in [`relay_answers`]' order.
/// # Errors
/// `write`'s own, an authorization refusal, or the flush's.
pub fn answer<C, S, G, T>(
    mut transport: TlsTransport<C>,
    guard: G,
    authorize: &(dyn Fn() -> io::Result<()> + Sync),
    write: impl FnOnce(&mut Answering<'_, C>) -> io::Result<T>,
) -> io::Result<T>
where
    C: std::ops::DerefMut + std::ops::Deref<Target = rustls::ConnectionCommon<S>>,
    S: rustls::SideData,
{
    let answered = write(&mut Answering {
        stream: transport.stream(),
        authorize,
    })
    .and_then(|t| {
        authorize()?;
        transport.stream().flush().map(|()| t)
    });
    drop(guard);
    end_without_waiting(transport, answered.is_ok());
    answered
}

pub(crate) fn end_without_waiting<C, S>(transport: TlsTransport<C>, clean: bool)
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

/// Hang detector for required queued and in-flight answer bytes after local EOF.
const RELAY_DRAIN_HANG: Duration = Duration::from_secs(60);

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
    /// the answer is over and dropped there in its order.
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
    /// queued, in-flight write completed, teardown.
    cv: Condvar,
}

impl<C> RelayShared<C> {
    fn lock(&self) -> MutexGuard<'_, RelayState<C>> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn drain_decrypted<C, S>(conn: &mut C, plain: &mut Vec<u8>, tmp: &mut [u8]) -> io::Result<bool>
where
    C: std::ops::DerefMut + std::ops::Deref<Target = rustls::ConnectionCommon<S>>,
    S: rustls::SideData,
{
    loop {
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

/// Relay between an authenticated TLS stream and a local `CtlStream` whose end ANSWERS.
/// # Errors
/// On setup failure (a socket clone, a thread that cannot start), an unexpected mid-stream I/O
/// error, an authorization refusal, or an unfinished required answer drain past its minute hang
/// detector.
/// A normal peer close — EOF, `close_notify`, a reset — is not an error.
pub fn relay_answers<C, S, G>(
    transport: TlsTransport<C>,
    local: CtlStream,
    guard: G,
    authorize: Arc<dyn Fn() -> io::Result<()> + Send + Sync>,
) -> io::Result<()>
where
    C: std::ops::DerefMut + std::ops::Deref<Target = rustls::ConnectionCommon<S>> + Send + 'static,
    S: rustls::SideData + 'static,
    G: Send + 'static,
{
    relay_answers_with::<C, S, G>(
        transport,
        local,
        guard,
        |_| {},
        Instant::now,
        || {},
        authorize,
    )
}

fn relay_answers_with<C, S, G>(
    transport: TlsTransport<C>,
    local: CtlStream,
    guard: G,
    mut writing: impl FnMut(&[u8]) + Send + 'static,
    drain_started: impl FnOnce() -> Instant + Send + 'static,
    mut draining: impl FnMut() + Send + 'static,
    authorize: Arc<dyn Fn() -> io::Result<()> + Send + Sync>,
) -> io::Result<()>
where
    C: std::ops::DerefMut + std::ops::Deref<Target = rustls::ConnectionCommon<S>> + Send + 'static,
    S: rustls::SideData + 'static,
    G: Send + 'static,
{
    let stream = transport.into_stream();
    let conn = stream.conn;
    let mut tcp_down = stream.sock;
    let _ = aterm_uds::keep_out_of_children(&tcp_down);
    let tcp_wr = aterm_uds::kept_out(|| tcp_down.try_clone())?;
    let tcp_up = aterm_uds::kept_out(|| tcp_down.try_clone())?;

    let _ = aterm_uds::keep_out_of_children(&local);
    let local_up = aterm_uds::kept_out(|| local.try_clone())?;
    let mut local_down = local;

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

    let wr = {
        let authorize = Arc::clone(&authorize);
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
                    return tcp_wr;
                }
                out.clear();
                while g.conn.wants_write() {
                    match g.conn.write_tls(&mut out) {
                        Ok(1..) => {}
                        Ok(0) | Err(_) => break,
                    }
                }
                g.inflight = true;
                drop(g);
                writing(&out);
                let sent = authorize()
                    .and_then(|()| tcp_wr.write_all(&out))
                    .and_then(|()| tcp_wr.flush());
                let mut g = shared.lock();
                g.inflight = false;
                if let Err(e) = sent {
                    if !g.done {
                        g.record_err(e);
                    }
                    g.done = true;
                    drop(g);
                    shared.cv.notify_all();
                    aterm_uds::hangup::hang_up(&tcp_wr);
                    return tcp_wr;
                }
                drop(g);
                shared.cv.notify_all();
            }
        })
    };
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

    let up = {
        let shared = Arc::clone(&shared);
        let uploader = std::thread::Builder::new().name("aterm-tls-in".to_owned());
        uploader.spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            let mut graceful = false;
            'up: loop {
                // A caller's copy of the local end can outlive this relay, whose return waits on
                // this thread: a read that missed that end's shutdown would wait on its own close.
                match aterm_uds::hangup::Awake(&local_up).read(&mut buf) {
                    Ok(0) => {
                        graceful = true;
                        break 'up;
                    }
                    Ok(n) => {
                        let mut written = 0usize;
                        let mut g = shared.lock();
                        while written < n {
                            if g.done {
                                break 'up;
                            }
                            match g.conn.writer().write(&buf[written..n]) {
                                Ok(0) => {
                                    g = shared.cv.wait(g).unwrap_or_else(|p| p.into_inner());
                                }
                                Ok(k) => {
                                    written += k;
                                    shared.cv.notify_all();
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
            let deadline = drain_started() + RELAY_DRAIN_HANG;
            let mut g = shared.lock();
            while !g.done && (g.conn.wants_write() || g.inflight) {
                let now = Instant::now();
                if now >= deadline {
                    g.record_err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "required TLS answer drain did not finish",
                    ));
                    break;
                }
                draining();
                g = shared
                    .cv
                    .wait_timeout(g, deadline - now)
                    .unwrap_or_else(|p| p.into_inner())
                    .0;
            }
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
            aterm_uds::hangup::hang_up(&tcp_up);
            (local_up, tcp_up)
        })
    };
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

    let mut tls_in = [0u8; 16 * 1024];
    let mut plain: Vec<u8> = Vec::with_capacity(16 * 1024);
    let mut tls_plain = [0u8; 16 * 1024];
    let mut fatal_down = false;
    let mut local_failed = false;

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
        && let Err(e) = authorize()
            .and_then(|()| local_down.write_all(&plain))
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
            Ok(0) => break,
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
            while !cursor.is_empty() && !clean_eof {
                match g.conn.read_tls(&mut cursor) {
                    Ok(1..) => {}
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
                shared.cv.notify_all();
            }
        }
        if !plain.is_empty()
            && let Err(e) = authorize()
                .and_then(|()| local_down.write_all(&plain))
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
    if local_failed {
        aterm_uds::hangup::hang_up(&local_down);
    } else {
        let _ = local_down.shutdown(std::net::Shutdown::Write);
    }
    let up_fds = up.join();
    {
        let mut g = shared.lock();
        g.done = true;
    }
    shared.cv.notify_all();
    aterm_uds::hangup::hang_up(&tcp_down);
    aterm_uds::hangup::hang_up(&local_down);
    let wr_fd = wr.join();
    drop((up_fds, wr_fd));

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
        // SAFETY: `tcp` keeps its descriptor open for the call, and `bytes` is a live slice of `bytes.len()` readable bytes.
        let sent =
            unsafe { libc::send(tcp.as_raw_fd(), bytes.as_ptr().cast(), bytes.len(), FLAGS) };
        match usize::try_from(sent) {
            Ok(n) if n > 0 => bytes = &bytes[n.min(bytes.len())..],
            Err(_) if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
            _ => return,
        }
    }
}

/// Windows has no per-call `MSG_DONTWAIT`: the socket (every copy of it) is
/// non-blocking for these sends and blocking again after, and each caller
/// hangs it up next. Without them the peer heard a FIN with no
/// `close_notify`, which rustls reads as a truncation (`UnexpectedEof`).
#[cfg(not(unix))]
fn send_without_waiting(tcp: &TcpStream, mut bytes: &[u8]) {
    if bytes.is_empty() || tcp.set_nonblocking(true).is_err() {
        return;
    }
    let mut socket = tcp;
    while !bytes.is_empty() {
        match socket.write(bytes) {
            Ok(n) if n > 0 => bytes = &bytes[n.min(bytes.len())..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            _ => break,
        }
    }
    let _ = tcp.set_nonblocking(false);
}

fn is_would_block(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

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
#[path = "tls_tests.rs"]
mod tests;
