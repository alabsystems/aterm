// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The global deadline and the authority guard hold for every socket call a TLS
//! stream makes, after the handshake as well as during it.
//!
//! rustls's `Read` for a stream loops `complete_io` until a whole record has
//! arrived, so one `Stream::read` is many socket reads. A timeout and a guard
//! applied once before that loop bound one of those reads; a peer that sends
//! the record a byte at a time restarts the clock with every byte and is never
//! asked about authority again. Most of these tests drive a REAL TLS session
//! (the handshake completes, against a pinned certificate) and then dribble
//! one record, so what they exercise is the established stream; one dribbles
//! the handshake itself, the deadline the harness network probe relies on.

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

/// Loopback credentials: a self-signed certificate and its key, public test
/// data (`testdata/README.md`).
const CERT: &[u8] = include_bytes!("testdata/loopback-cert.der");
const KEY: &[u8] = include_bytes!("testdata/loopback-key.pkcs8.der");

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

/// A TLS server that completes the handshake, then announces a 16 KiB
/// application record and sends it one byte per `step` for up to five minutes,
/// stopping at the first byte the closed client refuses. `dribbling` gets a
/// message once the first bytes are out.
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
        "loopback.test",
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

/// A peer that completes the TCP connect, claims a 16 KiB handshake record,
/// and then emits one byte at a time, `step` apart, for up to five minutes. It
/// never speaks TLS; the point is only that bytes keep ARRIVING. It stops at
/// the first byte the closed client refuses.
fn dribbling_handshake_peer(step: Duration) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let Ok((mut tcp, _)) = listener.accept() else {
            return;
        };
        // A handshake record header claiming 16384 bytes of body, so rustls
        // stays in `read_tls` waiting for a flight that never completes.
        if tcp.write_all(&[0x16, 0x03, 0x03, 0x40, 0x00]).is_err() {
            return;
        }
        let steps = Duration::from_secs(300).as_millis() / step.as_millis().max(1);
        for _ in 0..steps {
            if tcp.write_all(&[0x00]).is_err() || tcp.flush().is_err() {
                return;
            }
            std::thread::sleep(step);
        }
    });
    port
}

#[test]
fn a_dribbled_handshake_cannot_outlive_the_global_deadline() {
    // The deadline has to be enforced INSIDE the handshake, not just before
    // it: `complete_io` loops on `read_tls`, so a per-syscall timeout applied
    // once at the top bounds one read while every dribbled byte restarts the
    // clock. This peer dribbles for ~5 minutes against a 1 s budget; the
    // minute below is the hang detector.
    let port = dribbling_handshake_peer(Duration::from_millis(100));
    let tcp = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let (outcome_tx, outcome) = mpsc::channel();
    std::thread::spawn(move || {
        let started = Stream::start_tls(
            tcp,
            client_config(),
            "loopback.test",
            Arc::new(aterm_http::AlwaysAuthorized),
            Deadline::after(Duration::from_secs(1)),
        );
        let _ = outcome_tx.send(started.map(drop));
    });
    let result = outcome.recv_timeout(HANG_DETECTOR).unwrap_or_else(|_| {
        panic!("a TLS handshake was still waiting on a dribbling peer {HANG_DETECTOR:?} into a 1 s deadline")
    });
    let error = result.expect_err("a handshake that never finishes must hit the deadline");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut, "{error}");
}

#[test]
fn a_record_dribbled_after_the_handshake_cannot_outlive_the_global_deadline() {
    // At 50 ms a byte the record would take ~14 minutes; the 1.5 s deadline is
    // the bound and the minute below the hang detector. The deadline also covers
    // the handshake, which must finish inside it (`read_from_dribbler` expects it
    // to): a debug rustls handshake on loopback takes milliseconds.
    let (_dribbling, outcome) = read_from_dribbler(
        Arc::new(aterm_http::AlwaysAuthorized),
        Duration::from_millis(1500),
    );
    let result = outcome.recv_timeout(HANG_DETECTOR).unwrap_or_else(|_| {
        panic!(
            "a TLS read was still waiting on a dribbled record {HANG_DETECTOR:?} into a 1.5 s \
             deadline"
        )
    });
    let error = result.expect_err("a record that never completes cannot be read");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut, "{error}");
}

#[test]
fn authority_revoked_during_a_tls_read_stops_it() {
    // Only the guard can end this read: it must be consulted from inside
    // rustls's read loop, and once revoked the read stops.
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

/// A TLS server that completes the handshake, waits for the first plaintext
/// the client sends, answers `answer` and closes over everything else unread,
/// so the client's later writes meet a reset. Unix only, as its one test is.
#[cfg(unix)]
fn answering_tls_server(answer: &'static [u8]) -> u16 {
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
    std::thread::spawn(move || {
        let Ok((mut tcp, _)) = listener.accept() else {
            return;
        };
        let _ = tcp.set_read_timeout(Some(HANG_DETECTOR));
        // The answer leaves at once: under Nagle it would wait behind the
        // session tickets' unacknowledged segment, and the close's reset
        // would discard it unsent.
        let _ = tcp.set_nodelay(true);
        let Ok(mut conn) = rustls::ServerConnection::new(config) else {
            return;
        };
        let mut first = [0u8; 1];
        loop {
            match conn.reader().read(&mut first) {
                Ok(1) => break,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                _ => return,
            }
            if conn.complete_io(&mut tcp).is_err() {
                return;
            }
        }
        if conn.writer().write_all(answer).is_err() {
            return;
        }
        while conn.wants_write() {
            if conn.write_tls(&mut tcp).is_err() {
                return;
            }
        }
    });
    port
}

/// The TLS arm of an early answer (RFC 9112 §9.5): once a write has failed,
/// a read takes the records the peer sent before it closed, rather than
/// retrying the write rustls still holds and failing on it again. Unix only:
/// Windows discards the bytes a socket received once a reset arrives.
#[cfg(unix)]
#[test]
fn an_answer_sent_before_the_peer_closed_over_a_failed_write_is_read() {
    const ANSWER: &[u8] = b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 0\r\n\r\n";
    let port = answering_tls_server(ANSWER);
    let tcp = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut stream = Stream::start_tls(
        tcp,
        client_config(),
        "loopback.test",
        Arc::new(aterm_http::AlwaysAuthorized),
        Deadline::after(HANG_DETECTOR),
    )
    .expect("the handshake with the pinned loopback server completes");
    // Far more than the socket buffers hold: the server's close lands while
    // these writes are still going out.
    let chunk = vec![0u8; 64 * 1024];
    let failed = (0..1024)
        .find_map(|_| stream.write_all(&chunk).err())
        .expect("a write fails once the server has closed");
    assert_ne!(failed.kind(), std::io::ErrorKind::TimedOut, "{failed}");
    let mut got = Vec::new();
    let mut buf = [0u8; 256];
    while !got.ends_with(b"\r\n\r\n") {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(error) => panic!(
                "the answer was lost to {error} after {:?}",
                String::from_utf8_lossy(&got)
            ),
        }
    }
    assert_eq!(got, ANSWER);
}
