// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TLS 1.3 transport (rustls + the `ring` provider) for the L3 network drive.
//!
//! TLS gives three things the local Unix socket got for free: (1) an
//! authenticated, encrypted channel; (2) server identity via **certificate
//! fingerprint pinning** (a custom [`ServerCertVerifier`] — no CA/PKI, the dialer
//! pins the exact cert the endpoint record names); and (3) the **RFC 5705
//! keying-material exporter** — 32 bytes unique to this TLS session, symmetric on
//! both ends, that [`channel_bind`](crate::channel_bind) keys the capability HMAC
//! over. The exporter is what makes the capability resist an active MITM: a relay
//! that terminates one TLS leg holds a *different* exporter, so a captured tag
//! never transfers.
//!
//! The listener uses an **operator-provided** cert+key (standard server practice;
//! aterm does not mint certs). Tests use a self-signed fixture under
//! `src/testdata/`.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use aterm_uds::CtlStream;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, ServerConfig, ServerConnection,
    SignatureScheme, StreamOwned,
};
use sha2::{Digest, Sha256};

/// RFC 5705 exporter label — namespaces our keying material so it cannot collide
/// with any other exporter use on the same connection.
const EXPORTER_LABEL: &[u8] = b"EXPORTER-aterm-net-capability-v1";
/// The exporter (and the channel-binding HMAC) length.
pub const EXPORTER_LEN: usize = 32;

/// Install the `ring` crypto provider as the process default, once. Idempotent —
/// a second call (or a pre-installed provider) is ignored.
pub fn init_crypto() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Ignore the error: another caller may have already installed a provider.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// A fixed, valid SNI for dialing. Server identity is by certificate FINGERPRINT
/// ([`PinnedServerVerifier`]), not by name, so the SNI is irrelevant to security —
/// a constant keeps callers from having to construct a `rustls` type.
#[must_use]
pub fn fixed_server_name() -> ServerName<'static> {
    ServerName::try_from("aterm-net").expect("\"aterm-net\" is a valid DNS name")
}

/// SHA-256 of a certificate's DER — the fingerprint an endpoint record pins.
#[must_use]
pub fn cert_fingerprint(cert_der: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(cert_der);
    h.finalize().into()
}

fn io_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// A server [`ServerConfig`] from an operator-provided cert chain + key (both
/// DER). The key is PKCS#8.
///
/// # Errors
/// If the cert/key are malformed or incompatible.
pub fn server_config(cert_der: Vec<u8>, key_pkcs8_der: Vec<u8>) -> io::Result<Arc<ServerConfig>> {
    init_crypto();
    let certs = vec![CertificateDer::from(cert_der)];
    let key = PrivateKeyDer::try_from(key_pkcs8_der).map_err(io_err)?;
    // TLS 1.3 ONLY: a TLS 1.2 peer can negotiate a non-EMS master secret (RFC
    // 7627), whose RFC 5705 exporter is not bound to the full handshake
    // transcript — which would weaken the channel-binding the capability HMAC
    // keys over. Pinning 1.3 keeps the exporter transcript-bound.
    let cfg = ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(io_err)?;
    Ok(Arc::new(cfg))
}

/// A client [`ClientConfig`] that pins the server cert to `fingerprint` (SHA-256
/// of its DER) and otherwise verifies the TLS 1.3 handshake signature with the
/// `ring` provider — so the peer must BOTH present the pinned cert AND prove it
/// holds the matching private key.
#[must_use]
pub fn client_config(fingerprint: [u8; 32]) -> Arc<ClientConfig> {
    init_crypto();
    let verifier = PinnedServerVerifier {
        pin: fingerprint,
        supported: rustls::crypto::ring::default_provider().signature_verification_algorithms,
    };
    // TLS 1.3 ONLY (matches `server_config`): no downgrade to a non-EMS 1.2
    // session whose exporter would not be transcript-bound.
    let cfg = ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Arc::new(cfg)
}

/// Verifies the server cert by SHA-256 FINGERPRINT pinning (no CA/PKI), and the
/// handshake signature via the provider's algorithms (proving key possession).
#[derive(Debug)]
struct PinnedServerVerifier {
    pin: [u8; 32],
    supported: WebPkiSupportedAlgorithms,
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
        if cert_fingerprint(end_entity.as_ref()) == self.pin {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
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

/// A connected TLS stream plus its channel exporter — the input to
/// [`channel_bind`](crate::channel_bind). Generic over the rustls connection role
/// (`ServerConnection` / `ClientConnection`).
pub struct TlsTransport<C> {
    stream: StreamOwned<C, TcpStream>,
    exporter: [u8; EXPORTER_LEN],
}

impl<C> TlsTransport<C> {
    /// The RFC 5705 channel exporter — 32 bytes unique to this TLS session,
    /// identical on both ends, that the capability HMAC keys over.
    #[must_use]
    pub fn exporter(&self) -> &[u8] {
        &self.exporter
    }

    /// The connected TLS stream (Read + Write) for the post-handshake relay.
    pub fn stream(&mut self) -> &mut StreamOwned<C, TcpStream> {
        &mut self.stream
    }

    /// Consume into the raw TLS stream.
    pub fn into_stream(self) -> StreamOwned<C, TcpStream> {
        self.stream
    }
}

/// Server side: accept a TLS connection on an already-accepted `tcp`, completing
/// the handshake so the exporter is available.
///
/// # Errors
/// On a TLS or I/O failure during the handshake (`complete_io` errors on EOF).
pub fn accept(
    tcp: TcpStream,
    config: Arc<ServerConfig>,
) -> io::Result<TlsTransport<ServerConnection>> {
    let mut conn = ServerConnection::new(config).map_err(io_err)?;
    let mut tcp = tcp;
    while conn.is_handshaking() {
        conn.complete_io(&mut tcp)?;
    }
    let exporter: [u8; EXPORTER_LEN] = conn
        .export_keying_material([0u8; EXPORTER_LEN], EXPORTER_LABEL, None)
        .map_err(io_err)?;
    Ok(TlsTransport {
        stream: StreamOwned::new(conn, tcp),
        exporter,
    })
}

/// Client side: connect TLS over an already-connected `tcp` to a server whose cert
/// is pinned by `config`, completing the handshake so the exporter is available.
/// `server_name` is the SNI/name (any value; identity is by fingerprint, not name).
///
/// # Errors
/// On a TLS (incl. fingerprint mismatch) or I/O failure during the handshake.
pub fn connect(
    tcp: TcpStream,
    server_name: ServerName<'static>,
    config: Arc<ClientConfig>,
) -> io::Result<TlsTransport<ClientConnection>> {
    let mut conn = ClientConnection::new(config, server_name).map_err(io_err)?;
    let mut tcp = tcp;
    while conn.is_handshaking() {
        conn.complete_io(&mut tcp)?;
    }
    let exporter: [u8; EXPORTER_LEN] = conn
        .export_keying_material([0u8; EXPORTER_LEN], EXPORTER_LABEL, None)
        .map_err(io_err)?;
    Ok(TlsTransport {
        stream: StreamOwned::new(conn, tcp),
        exporter,
    })
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
    /// Fatal teardown, or graceful teardown after both half-closes complete.
    done: bool,
    /// Local request/input EOF has been encrypted, drained, and half-closed.
    upload_done: bool,
    /// Peer response/output EOF has been decrypted and flushed locally.
    download_done: bool,
    /// The writer thread holds TLS bytes it extracted from `conn` but has not yet
    /// pushed to the socket — the uploader's final drain must wait for them too.
    inflight: bool,
    /// The first UNEXPECTED error from any direction (a normal peer close is not
    /// recorded — it is the expected end of the relay).
    err: Option<io::Error>,
}

impl<C> RelayState<C> {
    fn record_err(&mut self, e: io::Error) {
        if !is_normal_close(&e) && self.err.is_none() {
            self.err = Some(e);
        }
    }
}

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

/// Full-duplex relay between an authenticated TLS stream and a local `CtlStream`
/// (the control socket), until both graceful half-closes complete (or a fatal
/// error tears both directions down). This is the network analog of
/// `proxy.rs`'s local splice: the listener bridges the verified remote driver to
/// its own control socket; the dialer bridges its local control client to the
/// remote.
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
/// # Errors
/// On setup failure (socket clone) or an unexpected mid-stream I/O error (a
/// normal peer close — EOF / `close_notify` / reset — is not an error).
pub fn relay<C, S>(transport: TlsTransport<C>, local: CtlStream) -> io::Result<()>
where
    C: std::ops::DerefMut + std::ops::Deref<Target = rustls::ConnectionCommon<S>> + Send + 'static,
    S: rustls::SideData + 'static,
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
            upload_done: false,
            download_done: false,
            inflight: false,
            err: None,
        }),
        cv: Condvar::new(),
    });

    // Writer: rustls -> TCP. The only thread that extracts queued TLS bytes, so
    // record order is preserved; the socket write happens with the lock released.
    let wr = {
        let shared = Arc::clone(&shared);
        let mut tcp_wr = tcp_wr;
        std::thread::spawn(move || {
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
                    let _ = tcp_wr.shutdown(std::net::Shutdown::Both);
                    return;
                }
                drop(g);
                shared.cv.notify_all(); // buffer space freed / drain progressed
            }
        })
    };

    // Uploader: local -> rustls plaintext. Blocking local read (teardown unblocks
    // it via `local_down.shutdown`); the lock is held only to buffer plaintext.
    let up = {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            // A local EOF is a DIRECTIONAL statement ("I have no more request to
            // send"), not "the conversation is over". A fatal error IS the latter.
            // The teardown below is the only place that difference is observable.
            let mut graceful = false;
            'up: loop {
                match local_up.read(&mut buf) {
                    Ok(0) => {
                        graceful = true;
                        break 'up; // local EOF -> half-close, drain, keep reading
                    }
                    Ok(n) => {
                        let mut written = 0usize;
                        let mut g = shared.lock();
                        while written < n {
                            if g.done {
                                return; // peer-side teardown already under way
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
            // Tell the peer THIS direction is finished, so it can stop waiting on
            // more request bytes while still sending its response.
            if graceful && !g.done {
                g.conn.send_close_notify();
                shared.cv.notify_all();
            }
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
            // A graceful half-close must NOT mark the relay done: the download
            // direction is still live and the peer's response may still be
            // arriving. Marking it done here (and shutting the socket down BOTH
            // ways below) truncated that response tail — a local proxy that
            // finished sending its request killed the reply it was waiting for.
            // The relay ends only once BOTH half-closes have landed; a FATAL
            // error (or teardown already under way) ends both at once.
            let half_close = graceful && !g.done;
            if half_close {
                g.upload_done = true;
                g.done = g.download_done;
            } else {
                g.done = true;
            }
            drop(g);
            shared.cv.notify_all();
            // Unblock the downloader's TCP read so it observes teardown — but on
            // a graceful EOF close only OUR write half, leaving the peer free to
            // keep sending. The downloader performs the final `Both` shutdown
            // when the response genuinely ends.
            let _ = tcp_up.shutdown(if half_close {
                std::net::Shutdown::Write
            } else {
                std::net::Shutdown::Both
            });
        })
    };

    // Downloader: TCP -> rustls -> local (this thread). The blocking socket read
    // holds no lock; ingest + decrypt hold it only for buffer work.
    let mut tls_in = [0u8; 16 * 1024];
    let mut plain: Vec<u8> = Vec::with_capacity(16 * 1024);
    let mut fatal_down = false;
    'down: loop {
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
            while !cursor.is_empty() {
                match g.conn.read_tls(&mut cursor) {
                    Ok(1..) => {}
                    // A slice source cannot fail, and `process_new_packets` after
                    // every feed keeps the deframer drained, so no-progress here
                    // means a record rustls cannot hold: fatal, not retryable.
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
            }
            loop {
                let mut tmp = [0u8; 4096];
                match g.conn.reader().read(&mut tmp) {
                    Ok(0) => {
                        clean_eof = true; // close_notify
                        break;
                    }
                    Ok(k) => plain.extend_from_slice(&tmp[..k]),
                    Err(e) if is_would_block(&e) => break, // no more plaintext yet
                    Err(e) if is_normal_close(&e) => {
                        clean_eof = true;
                        break;
                    }
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
            break;
        }
        if clean_eof {
            break;
        }
    }
    let graceful = {
        let mut g = shared.lock();
        if fatal_down || g.done {
            g.done = true;
            false
        } else {
            g.download_done = true;
            g.done = g.upload_done;
            true
        }
    };
    shared.cv.notify_all();
    // A graceful peer EOF is delivered to the local application's response
    // half only after every plaintext byte above was written and flushed. Its
    // eventual request-half EOF lets the uploader finish the reverse ACK.
    let _ = local_down.shutdown(if graceful {
        std::net::Shutdown::Write
    } else {
        std::net::Shutdown::Both
    });
    if !graceful {
        let _ = tcp_down.shutdown(std::net::Shutdown::Both);
    }
    let _ = up.join();
    {
        let mut g = shared.lock();
        g.done = true;
    }
    shared.cv.notify_all();
    let _ = tcp_down.shutdown(std::net::Shutdown::Both);
    let _ = local_down.shutdown(std::net::Shutdown::Both);
    let _ = wr.join();

    match shared.lock().err.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

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
    use crate::{Granted, present_capability, verify_capability};
    use aterm_session::EdgeToken;
    use std::net::{TcpListener, TcpStream};

    const TEST_CERT_DER: &[u8] = include_bytes!("testdata/cert.der");
    const TEST_KEY_DER: &[u8] = include_bytes!("testdata/key.pkcs8.der");

    fn test_server_name() -> ServerName<'static> {
        ServerName::try_from("aterm-net-test").unwrap()
    }

    #[test]
    fn tls_handshake_exporters_match_and_a_wrong_pin_is_rejected() {
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
    fn end_to_end_tls_capability_handshake_then_relays_a_control_exchange() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let token = EdgeToken::generate(); // Copy: both ends use the same token
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

        // Listener: accept TLS, verify the channel-bound capability, then relay.
        let srv = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let mut t = accept(tcp, scfg).unwrap();
            let exporter = t.exporter().to_vec();
            let granted = verify_capability(t.stream(), &exporter, |src, op| {
                (src == "driver-1" && op == "drive").then_some(token)
            })
            .unwrap();
            assert_eq!(
                granted,
                Granted {
                    src: "driver-1".into(),
                    op: "drive".into()
                }
            );
            relay(t, svc_a).unwrap();
        });

        // Dialer: connect TLS (pinned), present the capability, exchange, close.
        let ccfg = client_config(pin);
        let tcp = TcpStream::connect(addr).unwrap();
        let mut ct = connect(tcp, test_server_name(), ccfg).unwrap();
        let exporter = ct.exporter().to_vec();
        present_capability(ct.stream(), &exporter, "driver-1", "drive", &token).unwrap();

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
        // Regression: the uploader used to treat a transient WouldBlock from
        // write_all as fatal (record_err + break), dropping the just-read chunk
        // and tearing down a healthy relay the moment the non-blocking TLS socket
        // filled. A payload larger than the kernel send buffers forces that
        // backpressure mid-stream; the fix retries from the consumed offset and a
        // final draining flush delivers the tail, so every byte arrives in order.
        const PAYLOAD_LEN: usize = 1 << 20; // 1 MiB — well beyond any socket buffer
        let payload: Vec<u8> = (0..PAYLOAD_LEN).map(|i| (i % 251) as u8).collect();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let token = EdgeToken::generate();
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

        // Listener: accept TLS, verify the capability, then relay svc_a <-> TLS.
        let srv = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let mut t = accept(tcp, scfg).unwrap();
            let exporter = t.exporter().to_vec();
            verify_capability(t.stream(), &exporter, |src, op| {
                (src == "driver-1" && op == "drive").then_some(token)
            })
            .unwrap();
            relay(t, svc_a).unwrap();
        });

        // Dialer: connect, present the capability, then read the full payload back
        // off the relayed TLS stream and assert byte-for-byte fidelity.
        let ccfg = client_config(pin);
        let tcp = TcpStream::connect(addr).unwrap();
        let mut ct = connect(tcp, test_server_name(), ccfg).unwrap();
        let exporter = ct.exporter().to_vec();
        present_capability(ct.stream(), &exporter, "driver-1", "drive", &token).unwrap();

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

    #[test]
    fn relay_early_request_close_delivers_guarded_reply_but_not_late_ack() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let pin = cert_fingerprint(TEST_CERT_DER);
        let (svc_a, mut svc_b) = CtlStream::pair().unwrap();
        let mut response = (0..(128 * 1024))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        const NONCE: &[u8] = b"00112233445566778899aabbccddeeff";
        response.extend_from_slice(b"\nACK-CHALLENGE ");
        response.extend_from_slice(NONCE);
        response.push(b'\n');
        let expected_response = response.clone();
        let (ack_attempted_tx, ack_attempted_rx) = std::sync::mpsc::channel();

        let service = std::thread::spawn(move || {
            let mut request = Vec::new();
            svc_b.read_to_end(&mut request).unwrap();
            assert_eq!(request, b"one-shot request\n");
            svc_b.write_all(&response).unwrap();
            svc_b.flush().unwrap();
            ack_attempted_rx
                .recv_timeout(Duration::from_secs(10))
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
            relay(transport, svc_a).unwrap();
        });

        let tcp = TcpStream::connect(addr).unwrap();
        let mut client = connect(tcp, test_server_name(), client_config(pin)).unwrap();
        client.stream().write_all(b"one-shot request\n").unwrap();
        client.stream().flush().unwrap();
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
        client.stream().read_exact(&mut got).unwrap();
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
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();

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
            let transport = accept(tcp, scfg).unwrap();
            relay(transport, svc_a).unwrap();
        });

        let tcp = TcpStream::connect(addr).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut client = connect(tcp, test_server_name(), client_config(pin)).unwrap();
        client.stream().write_all(REQUEST).unwrap();
        client.stream().flush().unwrap();

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
    fn relay_round_trips_are_not_paced_by_a_poll_interval() {
        // Regression: the relay used to drive both directions with 20 ms
        // mutex+sleep polls, so every request/echo round-trip paid ~20-40 ms of
        // pure sleep — 300 sequential round-trips took >= ~6 s. With condvar
        // signaling and blocking socket reads each round-trip is bounded by real
        // I/O latency only, so the whole batch completes orders of magnitude
        // faster. The 3 s bound is generous for CI yet impossible under polling.
        const ROUND_TRIPS: usize = 300;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let token = EdgeToken::generate();
        let pin = cert_fingerprint(TEST_CERT_DER);

        // The "service" behind the control socket: a byte echo.
        let (svc_a, mut svc_b) = CtlStream::pair().unwrap();
        let echo = std::thread::spawn(move || {
            let mut buf = [0u8; 64];
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
            let mut t = accept(tcp, scfg).unwrap();
            let exporter = t.exporter().to_vec();
            verify_capability(t.stream(), &exporter, |src, op| {
                (src == "driver-1" && op == "drive").then_some(token)
            })
            .unwrap();
            relay(t, svc_a).unwrap();
        });

        let ccfg = client_config(pin);
        let tcp = TcpStream::connect(addr).unwrap();
        tcp.set_nodelay(true).unwrap();
        let mut ct = connect(tcp, test_server_name(), ccfg).unwrap();
        let exporter = ct.exporter().to_vec();
        present_capability(ct.stream(), &exporter, "driver-1", "drive", &token).unwrap();

        let started = Instant::now();
        let mut got = [0u8; 5];
        for _ in 0..ROUND_TRIPS {
            ct.stream().write_all(b"ping\n").unwrap();
            ct.stream().flush().unwrap();
            ct.stream().read_exact(&mut got).unwrap();
            assert_eq!(&got, b"ping\n");
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(3),
            "{ROUND_TRIPS} relayed round-trips took {elapsed:?} — the relay is \
             pacing traffic on a poll interval instead of waking on data"
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
    fn a_wrong_fingerprint_pin_rejects_the_handshake() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let srv = std::thread::spawn(move || {
            if let Ok((tcp, _)) = listener.accept() {
                let _ = accept(tcp, scfg); // expected to fail (client aborts)
            }
        });
        // Pin a DIFFERENT fingerprint -> the client must refuse the server cert.
        let mut wrong = cert_fingerprint(TEST_CERT_DER);
        wrong[0] ^= 0xff;
        let ccfg = client_config(wrong);
        let tcp = TcpStream::connect(addr).unwrap();
        let res = connect(tcp, test_server_name(), ccfg);
        assert!(
            res.is_err(),
            "a mismatched fingerprint pin must reject the handshake"
        );
        let _ = srv.join();
    }

    /// A GRACEFUL local EOF is a half-close of ONE direction, not the end of the
    /// conversation. The relay used to answer it with `Shutdown::Both`, so a
    /// local service that finished writing its reply killed the still-live
    /// request direction and any bytes the peer sent afterwards were lost.
    ///
    /// Deterministic by construction — every step is sequenced by a channel, so
    /// there is no sleep and no timing assumption. The service closes its WRITE
    /// half, announces that it has done so, and only THEN does the client send
    /// the second request. Under the old `Shutdown::Both` that second request can
    /// never arrive; with a directional half-close it must.
    #[test]
    fn graceful_local_eof_half_closes_without_killing_the_request_direction() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let pin = cert_fingerprint(TEST_CERT_DER);
        let (svc_a, mut svc_b) = CtlStream::pair().unwrap();

        const FIRST: &[u8] = b"first-request\n";
        const REPLY: &[u8] = b"the-whole-reply\n";
        const SECOND: &[u8] = b"second-request\n";

        let (write_half_closed_tx, write_half_closed_rx) = std::sync::mpsc::channel();
        let (second_seen_tx, second_seen_rx) = std::sync::mpsc::channel();

        let service = std::thread::spawn(move || {
            let mut first = vec![0u8; FIRST.len()];
            svc_b.read_exact(&mut first).unwrap();
            assert_eq!(first, FIRST, "the relay must deliver the first request");
            svc_b.write_all(REPLY).unwrap();
            svc_b.flush().unwrap();
            // The uploader (local -> TLS) now observes EOF. This is the graceful
            // half-close under test.
            svc_b.shutdown(std::net::Shutdown::Write).unwrap();
            write_half_closed_tx.send(()).unwrap();
            // The request direction must still be alive.
            let mut second = vec![0u8; SECOND.len()];
            let outcome = svc_b.read_exact(&mut second).map(|()| second);
            second_seen_tx.send(outcome.map_err(|e| e.kind())).unwrap();
        });
        let server = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let transport = accept(tcp, scfg).unwrap();
            let _ = relay(transport, svc_a);
        });

        let tcp = TcpStream::connect(addr).unwrap();
        let mut client = connect(tcp, test_server_name(), client_config(pin)).unwrap();
        client.stream().write_all(FIRST).unwrap();
        client.stream().flush().unwrap();

        let mut reply = vec![0u8; REPLY.len()];
        client.stream().read_exact(&mut reply).unwrap();
        assert_eq!(reply, REPLY, "the reply must survive intact");

        // Sequenced: the half-close has definitely happened before this send.
        write_half_closed_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("service should announce its write-half close");
        client.stream().write_all(SECOND).unwrap();
        client.stream().flush().unwrap();

        let seen = second_seen_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("service should report on the second request");
        assert_eq!(
            seen.as_deref(),
            Ok(SECOND),
            "a graceful local EOF must half-close only the response direction; \
             the request direction stayed open and delivered the second request"
        );

        drop(client);
        service.join().unwrap();
        server.join().unwrap();
    }
}
