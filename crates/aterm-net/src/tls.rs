// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TLS 1.3 transport (rustls + the `ring` provider) for the fleet ([`crate::fleet`]).

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

    /// The socket under the stream.
    #[inline]
    pub(crate) const fn tcp(&self) -> &TcpStream {
        &self.stream.sock
    }
}

/// Stream on which an [`answer`] is written, exposing I/O, timeouts, and peer address.
pub struct Answering<'a, C> {
    stream: &'a mut StreamOwned<C, TcpStream>,
}

impl<C> Answering<'_, C> {
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

/// Answer `transport` HERE, in [`relay_answers`]' order.
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

const RELAY_DRAIN_MAX: Duration = Duration::from_secs(5);

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
/// On setup failure (a socket clone, a thread that cannot start) or an unexpected mid-stream I/O
/// error (a normal peer close — EOF, `close_notify`, a reset — is not an error).
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
    let mut tcp_down = stream.sock;
    let tcp_wr = tcp_down.try_clone()?;
    let tcp_up = tcp_down.try_clone()?;

    let mut local_up = local.try_clone()?;
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
                    aterm_uds::hangup::hang_up(&tcp_wr);
                    return;
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
                match local_up.read(&mut buf) {
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
mod tests {
    use super::*;
    use rustls::pki_types::PrivateKeyDer;
    use rustls::{ClientConfig, ClientConnection, ServerConfig, ServerConnection};
    use std::net::{TcpListener, TcpStream};

    const TEST_CERT_DER: &[u8] = include_bytes!("testdata/cert.der");
    const TEST_KEY_DER: &[u8] = include_bytes!("testdata/key.pkcs8.der");

    const EXPORTER_LABEL: &[u8] = b"EXPORTER-aterm-net-test";

    fn io_err(e: impl std::fmt::Display) -> io::Error {
        io::Error::other(e.to_string())
    }

    fn server_config(cert_der: Vec<u8>, key_pkcs8_der: Vec<u8>) -> io::Result<Arc<ServerConfig>> {
        init_crypto();
        let key = PrivateKeyDer::try_from(key_pkcs8_der).map_err(io_err)?;
        let cfg = ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(cert_der)], key)
            .map_err(io_err)?;
        Ok(Arc::new(cfg))
    }

    fn client_config(fingerprint: [u8; 32]) -> Arc<ClientConfig> {
        init_crypto();
        let cfg = ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedServerVerifier::new(fingerprint)))
            .with_no_client_auth();
        Arc::new(cfg)
    }

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

    static LOOPBACK_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

        let srv = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let t = accept(tcp, scfg).unwrap();
            t.exporter().to_vec()
        });

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

        let srv = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let t = accept(tcp, scfg).unwrap();
            relay_answers(t, svc_a, ()).unwrap();
        });

        let ccfg = client_config(pin);
        let tcp = TcpStream::connect(addr).unwrap();
        let mut ct = connect(tcp, test_server_name(), ccfg).unwrap();

        ct.stream().write_all(b"ping\n").unwrap();
        ct.stream().flush().unwrap();
        let mut got = [0u8; 5];
        ct.stream().read_exact(&mut got).unwrap();
        assert_eq!(
            &got, b"ping\n",
            "the relay carried the control exchange round-trip"
        );

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
        const PAYLOAD_LEN: usize = 1 << 20;
        let payload: Vec<u8> = (0..PAYLOAD_LEN).map(|i| (i % 251) as u8).collect();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let pin = cert_fingerprint(TEST_CERT_DER);

        let (svc_a, mut svc_b) = CtlStream::pair().unwrap();
        let producer_payload = payload.clone();
        let producer = std::thread::spawn(move || {
            svc_b.write_all(&producer_payload).unwrap();
            svc_b.flush().unwrap();
        });

        let srv = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let t = accept(tcp, scfg).unwrap();
            relay_answers(t, svc_a, ()).unwrap();
        });

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
        let (piped_tx, piped) = mpsc::channel();
        let pipelining = std::thread::spawn(move || {
            let _ = piped_tx.send(ct.stream().write_all(&vec![b'p'; PIPELINED]));
        });
        let line = line.recv_timeout(HANG).expect("the server accepts");
        let filled = Instant::now() + HANG;
        while writable(&relay_side) {
            assert!(
                Instant::now() < filled,
                "the relay never filled its local end"
            );
            std::thread::yield_now();
        }
        assert!(aterm_uds::hangup::hang_up(&line), "the cut shuts the line");
        let over = relayed.recv_timeout(HANG);
        assert!(over.is_ok(), "the cut relay returns: {over:?}");
        assert!(dropped.try_recv().is_ok(), "its guard is gone");
        drop(relay_side);
        let failed = answered.recv_timeout(HANG);
        assert!(failed.is_ok(), "the local end's write fails: {failed:?}");
        drop(line);
        srv.join().unwrap();
        answering.join().unwrap();
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
        let (piped_tx, piped) = mpsc::channel();
        let pipelining = std::thread::spawn(move || {
            let _ = piped_tx.send(ct.stream().write_all(&vec![b'p'; PIPELINED]));
        });
        let filled = Instant::now() + HANG;
        while writable(&relay_side) {
            assert!(
                Instant::now() < filled,
                "the relay never filled its local end"
            );
            std::thread::yield_now();
        }
        drop(relay_side);
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
        tcp.set_read_timeout(Some(NET_TEST_LIVENESS_BOUND)).unwrap();
        let mut client = connect(tcp, test_server_name(), client_config(pin)).unwrap();
        client.stream().write_all(b"one-shot request\n").unwrap();
        client.stream().flush().unwrap();
        request_seen_rx
            .recv_timeout(NET_TEST_LIVENESS_BOUND)
            .expect("the service should announce the request");
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

            let stream = transport.stream();
            loop {
                let consumed = stream.conn.read_tls(&mut stream.sock).unwrap();
                assert!(consumed > 0, "the request must seed rustls' input buffer");
                let io = stream.conn.process_new_packets().unwrap();
                if io.plaintext_bytes_to_read() >= REQUEST.len() {
                    break;
                }
            }

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
            accept(tcp, scfg).err()
        });
        let mut wrong = cert_fingerprint(TEST_CERT_DER);
        wrong[0] ^= 0xff;
        let ccfg = client_config(wrong);
        let tcp = TcpStream::connect(addr).unwrap();
        let Err(err) = connect(tcp, test_server_name(), ccfg) else {
            panic!("a mismatched fingerprint pin must reject the handshake");
        };
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
        let heard = srv.join().unwrap().expect("the listener's handshake fails");
        assert!(
            heard
                .to_string()
                .contains("refused this listener's certificate"),
            "{heard}"
        );
    }

    const NET_TEST_LIVENESS_BOUND: Duration = Duration::from_secs(60);

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
            stream.write_all(&to_send[..FULL]).unwrap();
            for record in to_send[FULL..].chunks(SMALL) {
                stream.write_all(record).unwrap();
            }
            stream.flush().unwrap();
            written_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(NET_TEST_LIVENESS_BOUND);
            stream.conn.send_close_notify();
            let _ = stream.flush();
        });

        let tcp = TcpStream::connect(addr).unwrap();
        let transport = connect(tcp, test_server_name(), client_config(pin)).unwrap();
        written_rx.recv_timeout(NET_TEST_LIVENESS_BOUND).unwrap();
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
