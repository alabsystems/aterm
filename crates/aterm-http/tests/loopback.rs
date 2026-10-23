// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! END-TO-END evidence over a real loopback socket.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use aterm_http::{Client, Error, Guard, ProxyMode, Trust};

const HANG_DETECTOR: Duration = Duration::from_secs(60);

fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).unwrap_or(0) == 0 {
            break;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    if length > 0 {
        stream.read_exact(&mut body).unwrap();
    }
    (head, body)
}

fn stub(response: &'static [u8]) -> (String, mpsc::Receiver<(String, Vec<u8>)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let observed = read_request(&mut stream);
        let _ = tx.send(observed);
        let _ = stream.write_all(response);
        let _ = stream.flush();
    });
    (format!("http://127.0.0.1:{port}/api/chat"), rx)
}

fn client() -> Client {
    Client::new(Trust::PlatformVerifier, ProxyMode::Direct, HANG_DETECTOR)
}

#[test]
fn a_json_post_puts_the_expected_bytes_on_the_wire_and_parses_the_reply() {
    let (endpoint, rx) = stub(
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 22\r\n\r\n{\"message\":{\"a\":\"b\"}}\n",
    );
    let body = br#"{"model":"m","stream":false}"#;
    let response = client()
        .post(&endpoint)
        .header("Content-Type", "application/json")
        .header("Authorization", "Bearer secret-token")
        .limit(64 * 1024)
        .send(body)
        .expect("request succeeds");

    let (head, sent) = rx.recv_timeout(HANG_DETECTOR).unwrap();
    assert!(head.starts_with("POST /api/chat HTTP/1.1\r\n"), "{head}");
    assert!(head.contains("Host: 127.0.0.1:"), "{head}");
    assert!(
        head.contains("Content-Type: application/json\r\n"),
        "{head}"
    );
    assert!(
        head.contains("Authorization: Bearer secret-token\r\n"),
        "{head}"
    );
    assert!(
        head.contains(&format!("Content-Length: {}\r\n", body.len())),
        "{head}"
    );
    assert_eq!(sent, body);

    assert_eq!(response.status(), 200);
    assert!(response.is_success());
    assert_eq!(response.header("Content-Type").unwrap(), "application/json");
    assert_eq!(response.body(), b"{\"message\":{\"a\":\"b\"}}\n");
}

#[test]
fn a_get_and_a_put_put_their_methods_on_the_wire() {
    let (endpoint, rx) = stub(b"HTTP/1.1 200 OK\r\nETag: \"7\"\r\nContent-Length: 5\r\n\r\nvault");
    let response = client()
        .get(&format!("{endpoint}?after=6"))
        .limit(64)
        .send(&[])
        .expect("GET succeeds");
    let (head, sent) = rx.recv_timeout(HANG_DETECTOR).unwrap();
    assert!(
        head.starts_with("GET /api/chat?after=6 HTTP/1.1\r\n"),
        "{head}"
    );
    assert!(head.contains("Connection: close\r\n"), "{head}");
    assert!(
        !head.to_ascii_lowercase().contains("content-length"),
        "{head}"
    );
    assert!(sent.is_empty());
    assert_eq!(response.status(), 200);
    assert_eq!(response.header("etag"), Some("\"7\""));
    assert_eq!(response.body(), b"vault");

    let (endpoint, rx) = stub(b"HTTP/1.1 204 No Content\r\n\r\n");
    let body = b"alab-vault/1\n";
    let response = client()
        .put(&endpoint)
        .header("Content-Type", "text/plain; charset=utf-8")
        .limit(64)
        .send(body)
        .expect("PUT succeeds");
    let (head, sent) = rx.recv_timeout(HANG_DETECTOR).unwrap();
    assert!(head.starts_with("PUT /api/chat HTTP/1.1\r\n"), "{head}");
    assert!(
        head.contains(&format!("Content-Length: {}\r\n", body.len())),
        "{head}"
    );
    assert_eq!(sent, body);
    assert_eq!(response.status(), 204);
    assert!(response.body().is_empty());
}

#[test]
fn a_chunk_size_at_the_top_of_usize_is_an_error_not_a_panic() {
    let (endpoint, _rx) = stub(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nA\r\nffffffffffffffff\r\n",
    );
    let error = client()
        .post(&endpoint)
        .limit(4096)
        .send(b"{}")
        .expect_err("a chunk size that cannot fit must be refused");
    assert!(
        matches!(error, Error::TooLarge { limit: 4096 }),
        "{error:?}"
    );
}

#[derive(Debug)]
struct Revocable(AtomicBool);

impl Guard for Revocable {
    fn is_authorized(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[test]
fn authority_revoked_before_the_write_stops_the_body_leaving_the_process() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut seen = Vec::new();
            let _ = stream.read_to_end(&mut seen);
            let _ = tx.send(seen);
        } else {
            let _ = tx.send(Vec::new());
        }
    });

    let guard = Arc::new(Revocable(AtomicBool::new(false)));
    let error = client()
        .post(&format!("http://127.0.0.1:{port}/api/chat"))
        .guard(Arc::clone(&guard) as Arc<dyn Guard>)
        .limit(4096)
        .send(b"SENSITIVE-TERMINAL-CONTEXT")
        .expect_err("revoked authority must fail the request");
    assert!(error.is_revoked(), "{error:?}");

    let seen = rx
        .recv_timeout(HANG_DETECTOR)
        .expect("the stub reports what it read");
    assert!(
        !seen.windows(26).any(|w| w == b"SENSITIVE-TERMINAL-CONTEXT"),
        "revoked request still leaked its body: {:?}",
        String::from_utf8_lossy(&seen)
    );
}

#[test]
fn a_silent_server_hits_the_deadline_instead_of_hanging() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            std::thread::sleep(HANG_DETECTOR);
            drop(stream);
        }
    });
    let client = Client::new(
        Trust::PlatformVerifier,
        ProxyMode::Direct,
        Duration::from_millis(250),
    );
    let error = client
        .post(&format!("http://127.0.0.1:{port}/api/chat"))
        .limit(4096)
        .send(b"{}")
        .expect_err("a silent server must time out");
    assert!(
        matches!(&error, Error::Io(e) if e.kind() == std::io::ErrorKind::TimedOut),
        "{error:?}"
    );
}

fn capturing_stub() -> (u16, mpsc::Receiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let mut seen = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(read) = stream.read(&mut buf) {
            if read == 0 {
                break;
            }
            seen.extend_from_slice(&buf[..read]);
        }
        let _ = tx.send(seen);
    });
    (port, rx)
}

#[test]
fn an_https_endpoint_puts_a_client_hello_on_the_wire_and_never_the_body() {
    let secret = br#"{"model":"m","prompt":"CANARY-MUST-NEVER-APPEAR-IN-CLEARTEXT"}"#;
    let (port, rx) = capturing_stub();
    let error = Client::new(
        Trust::PlatformVerifier,
        ProxyMode::Direct,
        Duration::from_secs(1),
    )
    .post(&format!("https://127.0.0.1:{port}/api/chat"))
    .limit(4096)
    .send(secret)
    .expect_err("a peer that speaks no TLS cannot complete a handshake");

    let seen = rx
        .recv_timeout(HANG_DETECTOR)
        .expect("the stub must report what it saw");
    assert_eq!(
        seen.first(),
        Some(&0x16),
        "an https endpoint must open with a TLS handshake record, not \
         {:?}: {error:?}",
        String::from_utf8_lossy(&seen[..seen.len().min(16)])
    );
    assert!(
        !seen
            .windows(secret.len())
            .any(|window| window == secret.as_slice()),
        "the request body reached the socket in cleartext: {error:?}"
    );
    assert!(
        !seen.windows(5).any(|window| window == b"POST "),
        "the request head reached the socket in cleartext: {error:?}"
    );
}

fn dribbling_tls_peer(step: Duration, steps: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        if stream.write_all(&[0x16, 0x03, 0x03, 0x40, 0x00]).is_err() {
            return;
        }
        for _ in 0..steps {
            if stream.write_all(&[0x00]).is_err() || stream.flush().is_err() {
                return;
            }
            std::thread::sleep(step);
        }
    });
    format!("https://127.0.0.1:{port}/api/chat")
}

#[test]
fn a_dribbling_tls_peer_cannot_outlive_the_global_deadline() {
    let endpoint = dribbling_tls_peer(Duration::from_millis(100), 3_000);
    let client = Client::new(
        Trust::PlatformVerifier,
        ProxyMode::Direct,
        Duration::from_secs(1),
    );
    let started = std::time::Instant::now();
    let error = client
        .post(&endpoint)
        .limit(4096)
        .send(b"{}")
        .expect_err("a handshake that never finishes must hit the deadline");
    let elapsed = started.elapsed();
    assert!(
        elapsed < HANG_DETECTOR,
        "the handshake ran {elapsed:?} on a 1s budget: {error:?}"
    );
    assert!(matches!(error, Error::Io(_)), "{error:?}");
}

struct HttpsProxyEnv(String);

impl aterm_http::proxy::EnvSource for HttpsProxyEnv {
    fn var_os(&self, name: &str) -> Option<std::ffi::OsString> {
        (name == "HTTPS_PROXY").then(|| self.0.clone().into())
    }
}

#[test]
fn a_proxy_that_sends_an_interim_response_before_its_connect_answer_still_tunnels() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = stream.set_read_timeout(Some(HANG_DETECTOR));
        let (head, _) = read_request(&mut stream);
        let _ = stream
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 Connection established\r\n\r\n");
        let mut first = [0u8; 1];
        let read = stream.read(&mut first).unwrap_or(0);
        let _ = tx.send((head, (read == 1).then_some(first[0])));
    });
    let error = Client::new(
        Trust::PlatformVerifier,
        ProxyMode::Environment,
        HANG_DETECTOR,
    )
    .post("https://origin.test/api/chat")
    .limit(4096)
    .send_with_env(b"{}", &HttpsProxyEnv(format!("http://127.0.0.1:{port}")))
    .expect_err("the stub proxy speaks no TLS for the origin");
    let (head, first) = rx.recv_timeout(HANG_DETECTOR).unwrap();
    assert!(
        head.starts_with("CONNECT origin.test:443 HTTP/1.1\r\n"),
        "{head}"
    );
    assert_eq!(
        first,
        Some(0x16),
        "the client must open TLS through the tunnel the final 200 granted: {error:?}"
    );
}

#[test]
fn userinfo_in_the_proxy_url_authenticates_the_connect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = stream.set_read_timeout(Some(HANG_DETECTOR));
        let (head, _) = read_request(&mut stream);
        let _ = tx.send(head);
        let _ = stream
            .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n");
    });
    let error = Client::new(
        Trust::PlatformVerifier,
        ProxyMode::Environment,
        HANG_DETECTOR,
    )
    .post("https://origin.test/api/chat")
    .limit(4096)
    .send_with_env(
        b"{}",
        &HttpsProxyEnv(format!("http://alice:s3cret@127.0.0.1:{port}")),
    )
    .expect_err("the stub proxy refuses the tunnel");
    let head = rx.recv_timeout(HANG_DETECTOR).unwrap();
    assert!(
        head.starts_with("CONNECT origin.test:443 HTTP/1.1\r\n"),
        "{head}"
    );
    assert!(
        head.contains("\r\nProxy-Authorization: Basic YWxpY2U6czNjcmV0\r\n"),
        "{head}"
    );
    assert!(matches!(error, Error::Protocol(_)), "{error:?}");
    assert!(!error.to_string().contains("s3cret"), "{error}");
}

#[test]
fn an_https_proxy_is_refused_before_any_byte_reaches_it() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let error = Client::new(
        Trust::PlatformVerifier,
        ProxyMode::Environment,
        Duration::from_secs(2),
    )
    .post("https://origin.test/api/chat")
    .send_with_env(b"{}", &HttpsProxyEnv(format!("https://127.0.0.1:{port}")))
    .expect_err("an https:// proxy is refused");
    assert!(
        matches!(&error, Error::Invalid(message) if message.contains("HTTPS_PROXY")),
        "{error:?}"
    );
    listener.set_nonblocking(true).unwrap();
    match listener.accept() {
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        other => panic!("the client connected to an https:// proxy: {other:?}"),
    }
}

#[test]
fn a_refused_port_is_an_error_not_a_panic() {
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let error = client()
        .post(&format!("http://127.0.0.1:{port}/api/chat"))
        .limit(4096)
        .send(b"{}")
        .expect_err("connection refused");
    assert!(!error.is_revoked(), "{error:?}");
}

#[test]
fn an_unusable_endpoint_fails_before_any_socket_is_opened() {
    for bad in [
        "ftp://127.0.0.1/x",
        "http://user:pass@127.0.0.1/x",
        "not-a-url",
    ] {
        let error = client()
            .post(bad)
            .send(b"{}")
            .expect_err("must reject the endpoint");
        assert!(matches!(error, Error::Invalid(_)), "{bad}: {error:?}");
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(
                !rendered.contains("pass"),
                "{bad}: the error echoed the credential: {rendered}"
            );
        }
    }
}
