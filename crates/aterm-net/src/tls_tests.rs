// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

fn accept(tcp: TcpStream, config: Arc<ServerConfig>) -> io::Result<TlsTransport<ServerConnection>> {
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

fn completed_pair() -> (
    TlsTransport<ClientConnection>,
    TlsTransport<ServerConnection>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
    let server = std::thread::spawn(move || {
        let (tcp, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        tcp.set_read_timeout(Some(NET_TEST_LIVENESS_BOUND)).unwrap();
        tcp.set_write_timeout(Some(NET_TEST_LIVENESS_BOUND))
            .unwrap();
        accept(tcp, scfg).unwrap()
    });
    let tcp = TcpStream::connect(addr).unwrap();
    tcp.set_read_timeout(Some(NET_TEST_LIVENESS_BOUND)).unwrap();
    tcp.set_write_timeout(Some(NET_TEST_LIVENESS_BOUND))
        .unwrap();
    let client = connect(
        tcp,
        test_server_name(),
        client_config(cert_fingerprint(TEST_CERT_DER)),
    )
    .unwrap();
    let server = server.join().unwrap();
    for socket in [&client.stream.sock, &server.stream.sock] {
        socket.set_read_timeout(None).unwrap();
        socket.set_write_timeout(None).unwrap();
    }
    (client, server)
}

#[test]
fn bounded_tls_progress_cannot_extend_the_absolute_exchange_deadline() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let _serial = serialize_loopback();
    let (client, mut server) = completed_pair();
    let before = Instant::now();
    let until = before + NET_TEST_LIVENESS_BOUND * 10;
    let expired = Arc::new(AtomicBool::new(false));
    let clock_expired = Arc::clone(&expired);
    let (worker_tx, worker_rx) = std::sync::mpsc::channel();
    let watchdog = crate::slots::HandshakeWatchdog::arm_until_with_clock(
        &client.stream.sock,
        until,
        move || {
            worker_tx.send(std::thread::current()).unwrap();
            if clock_expired.load(Ordering::Acquire) {
                until
            } else {
                before
            }
        },
    )
    .unwrap();
    let mut stream = BoundedStream {
        preparation: None,
        watchdog,
        stream: client.stream,
        until,
    };
    let worker = worker_rx.recv_timeout(NET_TEST_LIVENESS_BOUND).unwrap();
    // Real TLS progress succeeds before the controlled clock reaches the deadline.
    for byte in b"drip" {
        server.stream().write_all(&[*byte]).unwrap();
        server.stream().flush().unwrap();
        let mut received = [0];
        stream.read_exact(&mut received).unwrap();
        assert_eq!(received, [*byte]);
    }

    let (reading_tx, reading_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        reading_tx.send(()).unwrap();
        let read = stream.read_exact(&mut [0; 1024]);
        let write = stream.write_all(b"late");
        let flush = stream.flush();
        done_tx.send((read, write, flush)).unwrap();
    });
    reading_rx.recv_timeout(NET_TEST_LIVENESS_BOUND).unwrap();
    expired.store(true, Ordering::Release);
    worker.unpark();
    let ended = done_rx.recv_timeout(NET_TEST_LIVENESS_BOUND);
    // Also release the real reader on regression so the test leaves no blocked thread.
    aterm_uds::hangup::hang_up(&server.stream.sock);
    reader.join().unwrap();
    let (read, write, flush) = ended.expect("the absolute deadline interrupts read_exact");
    for outcome in [read, write, flush] {
        assert_eq!(outcome.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }
}

#[test]
fn bounded_tls_rejects_an_already_expired_phase_before_io() {
    let _serial = serialize_loopback();
    let (client, _server) = completed_pair();
    let mut stream = client.into_bounded_stream(Instant::now()).unwrap();
    assert_eq!(
        stream.read(&mut [0]).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(
        stream.write(b"late").unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(stream.flush().unwrap_err().kind(), io::ErrorKind::TimedOut);
}

#[test]
fn bounded_tls_exchanges_data_and_cancels_its_owned_watchdog_on_drop() {
    let _serial = serialize_loopback();
    let (client, mut server) = completed_pair();
    let mut stream = client
        .into_bounded_stream(Instant::now() + NET_TEST_LIVENESS_BOUND * 10)
        .unwrap();
    stream.write_all(b"ping").unwrap();
    stream.flush().unwrap();
    let mut request = [0; 4];
    server.stream().read_exact(&mut request).unwrap();
    assert_eq!(&request, b"ping");
    server.stream().write_all(b"pong").unwrap();
    server.stream().flush().unwrap();
    let mut reply = [0; 4];
    stream.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"pong");

    #[cfg(unix)]
    let joined_while_open = {
        use std::os::fd::AsRawFd as _;
        use std::sync::atomic::{AtomicBool, Ordering};

        let fd = stream.stream.sock.as_raw_fd();
        let observed = Arc::new(AtomicBool::new(false));
        let probe = Arc::clone(&observed);
        stream.watchdog.observe_join(move || {
            // SAFETY: the wrapper owns this descriptor throughout watchdog teardown.
            assert_ne!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
            probe.store(true, Ordering::Release);
        });
        observed
    };
    let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
    let dropper = std::thread::spawn(move || {
        drop(stream);
        dropped_tx.send(()).unwrap();
    });
    dropped_rx
        .recv_timeout(NET_TEST_LIVENESS_BOUND)
        .expect("Drop cancels and joins rather than waiting for the exchange deadline");
    dropper.join().unwrap();
    #[cfg(unix)]
    assert!(joined_while_open.load(std::sync::atomic::Ordering::Acquire));

    // Preparation cancellation closes the exact live socket as well as stopping
    // future reads. Its watchdog remains owned until the stream is dropped.
    let (client, mut server) = completed_pair();
    let control = Arc::new(PreparationControl::new());
    let mut stream = client
        .into_preparing_stream(
            Instant::now() + NET_TEST_LIVENESS_BOUND * 10,
            Arc::clone(&control),
        )
        .unwrap();
    let (entered, entering) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        entered.send(()).unwrap();
        let result = stream.read(&mut [0]);
        released.recv().unwrap();
        drop(stream);
        result
    });
    entering.recv_timeout(NET_TEST_LIVENESS_BOUND).unwrap();
    control.cancel();
    server
        .stream()
        .sock
        .set_read_timeout(Some(NET_TEST_LIVENESS_BOUND))
        .unwrap();
    let ended = server.stream().read(&mut [0]);
    // Cleanup also runs if the cancellation assertion would fail, so the real
    // reader never stays behind in a failed fixture.
    if let Some(phase) = control.phase.lock().unwrap().as_ref() {
        phase.evict();
    }
    release.send(()).unwrap();
    let result = worker.join().unwrap();
    assert!(
        matches!(&ended, Ok(0))
            || ended.as_ref().is_err_and(|error| !matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            )),
        "the cancelled socket stays open: {ended:?}"
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::ConnectionAborted);
    assert!(
        control.phase.lock().unwrap().is_none(),
        "no stale cancellation phase"
    );
}

#[test]
fn tls_handshake_exporters_match() {
    let _serial = serialize_loopback();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();

    let srv = std::thread::spawn(move || {
        let (tcp, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
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
    assert_ne!(
        client_exporter, [0u8; EXPORTER_LEN],
        "exporter is real key material"
    );
}

#[test]
fn a_required_native_tls_answer_waits_for_its_inflight_writer() {
    let _serial = serialize_loopback();
    for age in [
        Duration::from_secs(6),
        Duration::from_secs(61),
        Duration::ZERO,
    ] {
        let expired = age >= aterm_spec::wait::MUST_HAPPEN;
        let (mut client, transport) = completed_pair();
        client
            .stream()
            .sock
            .set_read_timeout(Some(NET_TEST_LIVENESS_BOUND))
            .unwrap();
        let (local, mut responder) = CtlStream::pair().unwrap();
        let expected = b"required native TLS response\n";
        responder.write_all(expected).unwrap();
        responder.shutdown(std::net::Shutdown::Write).unwrap();
        let (proceed, waited) = std::sync::mpsc::sync_channel(1);
        let rescue = proceed.clone();
        let mut waited = Some(waited);
        let mut proceed = Some(proceed);
        let observed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let blocked = Arc::clone(&observed);
        let relayed = std::thread::spawn(move || {
            relay_answers_with(
                transport,
                local,
                (),
                move |wire| {
                    assert!(
                        !wire.is_empty(),
                        "the native TLS writer extracted a real record"
                    );
                    if let Some(waited) = waited.take() {
                        waited.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
                    }
                },
                move || Instant::now() - age,
                move || {
                    if let Some(proceed) = proceed.take() {
                        blocked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        proceed.send(()).unwrap();
                    }
                },
                Arc::new(|| Ok(())),
            )
        });
        let mut bytes = Vec::new();
        let read = client.stream().read_to_end(&mut bytes);
        // A truly expired drain never releases the writer; cleanup does so only after
        // the actual TLS peer has observed closure, before joining and asserting.
        let _ = rescue.try_send(());
        drop(client);
        drop(responder);
        let outcome = relayed.join().unwrap();
        assert_eq!(
            observed.load(std::sync::atomic::Ordering::SeqCst),
            usize::from(!expired)
        );
        if expired {
            assert!(read.is_err());
            assert!(
                bytes.is_empty(),
                "a cancelled native writer emits no answer later"
            );
            assert_eq!(outcome.unwrap_err().kind(), io::ErrorKind::TimedOut);
        } else {
            assert!(
                read.is_ok(),
                "a scheduled native writer is not a truncated answer: {read:?}"
            );
            assert!(outcome.is_ok(), "{outcome:?}");
            assert_eq!(bytes, expected);
        }
    }
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
        let (tcp, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        let t = accept(tcp, scfg).unwrap();
        relay_answers(t, svc_a, (), Arc::new(|| Ok(()))).unwrap();
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
        let (tcp, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        let t = accept(tcp, scfg).unwrap();
        relay_answers(t, svc_a, (), Arc::new(|| Ok(()))).unwrap();
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
        let (tcp, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        tcp.set_read_timeout(Some(HANG)).unwrap();
        let t = accept(tcp, scfg).unwrap();
        let guard = HeldGuard {
            entered: entered_tx,
            gate,
        };
        answer(t, guard, &|| Ok(()), |stream| {
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
fn custody_revocation_blocks_content_before_tls_writes_it() {
    let _serial = serialize_loopback();
    for revoke in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let config = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let server = std::thread::spawn(move || {
            let (tcp, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            tcp.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
            let transport = accept(tcp, config).unwrap();
            let current = std::sync::atomic::AtomicBool::new(true);
            answer(
                transport,
                (),
                &|| {
                    if current.load(std::sync::atomic::Ordering::Acquire) {
                        Ok(())
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "custody changed",
                        ))
                    }
                },
                |stream| {
                    let mut request = [0; 1];
                    stream.read_exact(&mut request)?;
                    current.store(!revoke, std::sync::atomic::Ordering::Release);
                    stream.write_all(b"content")
                },
            )
        });
        let tcp = TcpStream::connect(addr).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        let mut client = connect(
            tcp,
            test_server_name(),
            client_config(cert_fingerprint(TEST_CERT_DER)),
        )
        .unwrap();
        client.stream().write_all(b"?").unwrap();
        client.stream().flush().unwrap();
        let mut content = Vec::new();
        let read = client.stream().read_to_end(&mut content);
        let served = server.join().unwrap();
        if revoke {
            assert!(content.is_empty());
            assert_eq!(served.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        } else {
            read.unwrap();
            served.unwrap();
            assert_eq!(content, b"content");
        }
    }
}

#[test]
fn custody_revocation_blocks_a_relays_prepared_content() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let _serial = serialize_loopback();
    for revoke in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let config = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
        let current = Arc::new(AtomicBool::new(true));
        let changed = Arc::clone(&current);
        let (local, mut service) = CtlStream::pair().unwrap();
        let prepared = std::thread::spawn(move || {
            let mut request = [0; 1];
            service.read_exact(&mut request).unwrap();
            changed.store(!revoke, Ordering::Release);
            let _ = service.write_all(b"content").and_then(|()| service.flush());
        });
        let server = std::thread::spawn(move || {
            let (tcp, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            let transport = accept(tcp, config).unwrap();
            relay_answers(
                transport,
                local,
                (),
                Arc::new(move || {
                    if current.load(Ordering::Acquire) {
                        Ok(())
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "custody changed",
                        ))
                    }
                }),
            )
        });
        let tcp = TcpStream::connect(addr).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        let mut client = connect(
            tcp,
            test_server_name(),
            client_config(cert_fingerprint(TEST_CERT_DER)),
        )
        .unwrap();
        client.stream().write_all(b"?").unwrap();
        client.stream().flush().unwrap();
        let mut content = Vec::new();
        let read = client.stream().read_to_end(&mut content);
        prepared.join().unwrap();
        let served = server.join().unwrap();
        if revoke {
            assert!(content.is_empty());
            assert_eq!(served.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        } else {
            read.unwrap();
            served.unwrap();
            assert_eq!(content, b"content");
        }
    }
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
        let (tcp, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        let t = accept(tcp, scfg).unwrap();
        let guard = HeldGuard {
            entered: entered_tx,
            gate,
        };
        let _ = relayed_tx.send(relay_answers(t, svc_a, guard, Arc::new(|| Ok(()))));
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
        let (tcp, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        line_tx.send(tcp.try_clone().unwrap()).unwrap();
        let t = accept(tcp, scfg).unwrap();
        let _ = relayed_tx.send(relay_answers(
            t,
            svc_a,
            SaysDropped(dropped_tx),
            Arc::new(|| Ok(())),
        ));
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
        aterm_uds::deadline::poll_yield();
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
        let (tcp, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        let t = accept(tcp, scfg).unwrap();
        let _ = relayed_tx.send(relay_answers(
            t,
            svc_a,
            SaysDropped(dropped_tx),
            Arc::new(|| Ok(())),
        ));
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
        aterm_uds::deadline::poll_yield();
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
        let (tcp, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        let t = accept(tcp, scfg).unwrap();
        let _ = relayed_tx.send(relay_answers(t, svc_a, guard, Arc::new(|| Ok(()))));
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
        let (tcp, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        let transport = accept(tcp, scfg).unwrap();
        relay_answers(transport, svc_a, (), Arc::new(|| Ok(()))).unwrap();
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
    client
        .stream()
        .read_exact(&mut got)
        .unwrap_or_else(|error| {
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

fn assert_tls_buffered_relay_conformance(buffered_bytes: usize, service_received: bool) {
    let model = aterm_models_session::tls_buffered_relay_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let label = "TlsBufferedRelay(relay_answers)";
    let mut parked = model.init_state();
    parked.insert("buffered", i64::from(buffered_bytes > 0));
    assert!(
        !buggy.action_enabled("DrainBuffered", &parked),
        "Buggy=1 must refuse DrainBuffered on pre-buffered plaintext"
    );
    let mut drained = parked.clone();
    drained.insert("buffered", 0);
    drained.insert("relayed", i64::from(buffered_bytes > 0));
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &parked,
        &drained,
        Some("DrainBuffered"),
        label,
    );
    assert!(ok, "DrainBuffered transition rejected: {why}");
    let mut served = drained.clone();
    served.insert("service_waiting", i64::from(!service_received));
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &drained,
        &served,
        Some("ServiceRecv"),
        label,
    );
    assert!(ok, "ServiceRecv transition rejected: {why}");
    assert!(model.action_enabled("Done", &served));
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
        request == REQUEST
    });
    let server = std::thread::spawn(move || {
        let (tcp, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        tcp.set_read_timeout(Some(NET_TEST_LIVENESS_BOUND)).unwrap();
        let mut transport = accept(tcp, scfg).unwrap();
        server_ready_tx.send(()).unwrap();
        request_flushed_rx
            .recv_timeout(NET_TEST_LIVENESS_BOUND)
            .unwrap();

        let stream = transport.stream();
        let buffered_bytes = loop {
            let consumed = stream.conn.read_tls(&mut stream.sock).unwrap();
            assert!(consumed > 0, "the request must seed rustls' input buffer");
            let io = stream.conn.process_new_packets().unwrap();
            if io.plaintext_bytes_to_read() >= REQUEST.len() {
                break io.plaintext_bytes_to_read();
            }
        };

        relay_answers(transport, svc_a, (), Arc::new(|| Ok(()))).unwrap();
        buffered_bytes
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
    let service_received = service.join().unwrap();
    let buffered_bytes = server.join().unwrap();
    assert_tls_buffered_relay_conformance(buffered_bytes, service_received);
}

#[test]
fn a_wrong_fingerprint_pin_rejects_the_handshake() {
    let _serial = serialize_loopback();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let scfg = server_config(TEST_CERT_DER.to_vec(), TEST_KEY_DER.to_vec()).unwrap();
    let srv = std::thread::spawn(move || {
        let (tcp, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .ok()?;
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
        let (tcp, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
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
    let relayed =
        std::thread::spawn(move || relay_answers(transport, local_relay, (), Arc::new(|| Ok(()))));
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
