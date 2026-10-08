// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The global deadline and the authority guard hold for EVERY socket call a
//! TLS stream makes — after the handshake as well as during it.
//!
//! rustls's `Read` for a stream loops `complete_io` until a whole record has
//! arrived, so one `Stream::read` is many socket reads. A timeout and a guard
//! applied once before that loop bound one of those reads; a peer that sends
//! the record a byte at a time restarts the clock with every byte and is never
//! asked about authority again. These tests drive a REAL TLS session (the
//! handshake completes, against a pinned certificate) and then dribble one
//! record, so what they exercise is the established stream the response is
//! read through, not the handshake `loopback.rs` already covers.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use aterm_http::stream::{Deadline, Guard, Stream};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

/// A wait for something that must happen is a hang detector, never a latency budget.
const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// aterm-net's loopback credentials: a self-signed certificate and its key,
/// public test data (`crates/aterm-net/src/testdata/README.md`).
const CERT: &[u8] = include_bytes!("../../aterm-net/src/testdata/cert.der");
const KEY: &[u8] = include_bytes!("../../aterm-net/src/testdata/key.pkcs8.der");

/// Accepts exactly [`CERT`] (the handshake signature is still checked), so the
/// session these tests run over is a real one without a CA in the picture.
#[derive(Debug)]
struct PinnedTestCert(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for PinnedTestCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() == CERT {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::UnknownIssuer,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    aterm_http::tls::init_crypto();
    rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .expect("init_crypto installs a provider")
}

fn client_config() -> Arc<rustls::ClientConfig> {
    Arc::new(
        rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .expect("default protocol versions")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedTestCert(provider())))
            .with_no_client_auth(),
    )
}

/// A TLS server that completes the handshake, then announces an application
/// record of 16 KiB and sends it ONE BYTE AT A TIME, `step` apart, for up to
/// five minutes. `dribbling` gets a message once the first bytes are out. The
/// peer stops at the first byte the closed client refuses.
fn dribbling_tls_server(step: Duration) -> (u16, mpsc::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .expect("default protocol versions")
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(CERT)],
                PrivateKeyDer::try_from(KEY).expect("a PKCS#8 key"),
            )
            .expect("the loopback credentials pair"),
    );
    let (dribbling, dribbling_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut tcp, _)) = listener.accept() else {
            return;
        };
        let _ = tcp.set_read_timeout(Some(HANG_DETECTOR));
        let Ok(mut conn) = rustls::ServerConnection::new(config) else {
            return;
        };
        while conn.is_handshaking() {
            if conn.complete_io(&mut tcp).is_err() {
                return;
            }
        }
        // Flush anything the handshake left queued (session tickets).
        while conn.wants_write() {
            if conn.complete_io(&mut tcp).is_err() {
                return;
            }
        }
        // An application-data record header claiming 16 KiB of body: the
        // client's rustls keeps reading until the whole record is in.
        if tcp.write_all(&[0x17, 0x03, 0x03, 0x40, 0x00]).is_err() {
            return;
        }
        let steps = Duration::from_secs(300).as_millis() / step.as_millis().max(1);
        for sent in 0..steps {
            if tcp.write_all(&[0x00]).is_err() || tcp.flush().is_err() {
                return;
            }
            if sent == 3 {
                let _ = dribbling.send(());
            }
            std::thread::sleep(step);
        }
    });
    (port, dribbling_rx)
}

/// A guard that can be revoked, and counts how often it is consulted.
#[derive(Debug)]
struct Revocable {
    allowed: AtomicBool,
    asked: AtomicUsize,
}

impl Guard for Revocable {
    fn is_authorized(&self) -> bool {
        self.asked.fetch_add(1, Ordering::AcqRel);
        self.allowed.load(Ordering::Acquire)
    }
}

/// Handshake with the dribbling server, then read on a thread of its own; the
/// receiver yields the read's outcome.
fn read_from_dribbler(
    guard: Arc<dyn Guard>,
    deadline: Duration,
) -> (mpsc::Receiver<()>, mpsc::Receiver<std::io::Result<usize>>) {
    let (port, dribbling) = dribbling_tls_server(Duration::from_millis(50));
    let tcp = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut stream = Stream::start_tls(
        tcp,
        client_config(),
        "aterm-net-test",
        guard,
        Deadline::after(deadline),
    )
    .expect("the handshake with the pinned loopback server completes");
    let (outcome_tx, outcome) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 64];
        let _ = outcome_tx.send(stream.read(&mut buf));
    });
    (dribbling, outcome)
}

#[test]
fn a_record_dribbled_after_the_handshake_cannot_outlive_the_global_deadline() {
    // Each byte arrives 50 ms after the last, so a timeout armed once before
    // rustls's read loop never fires: the record would take ~14 minutes. The
    // 3 s deadline is the bound; the minute below is the hang detector.
    let (_dribbling, outcome) = read_from_dribbler(
        Arc::new(aterm_http::AlwaysAuthorized),
        Duration::from_secs(3),
    );
    let result = outcome.recv_timeout(HANG_DETECTOR).unwrap_or_else(|_| {
        panic!(
            "a TLS read was still waiting on a dribbled record {HANG_DETECTOR:?} into a 3 s \
             deadline"
        )
    });
    let error = result.expect_err("a record that never completes cannot be read");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut, "{error}");
}

#[test]
fn authority_revoked_during_a_tls_read_stops_it() {
    // The deadline is far away; only the guard can end this read. The guard
    // must be CONSULTED while the record dribbles in — i.e. from inside
    // rustls's read loop, not only once before it — and once it is, it is
    // revoked and the read must stop.
    let guard = Arc::new(Revocable {
        allowed: AtomicBool::new(true),
        asked: AtomicUsize::new(0),
    });
    let (dribbling, outcome) = read_from_dribbler(
        Arc::clone(&guard) as Arc<dyn Guard>,
        Duration::from_secs(3600),
    );
    dribbling
        .recv_timeout(HANG_DETECTOR)
        .expect("the server starts dribbling");
    let asked_before = guard.asked.load(Ordering::Acquire);
    let started = std::time::Instant::now();
    while guard.asked.load(Ordering::Acquire) == asked_before {
        assert!(
            started.elapsed() < HANG_DETECTOR,
            "a TLS read took {HANG_DETECTOR:?} of dribbled bytes without consulting its \
             authority once"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    guard.allowed.store(false, Ordering::Release);
    let result = outcome.recv_timeout(HANG_DETECTOR).unwrap_or_else(|_| {
        panic!("a TLS read kept going {HANG_DETECTOR:?} after its authority was revoked")
    });
    let error = result.expect_err("a revoked read cannot succeed");
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied,
        "{error}"
    );
}
