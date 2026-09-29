// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! END-TO-END evidence over a real loopback socket.
//!
//! The endpoint this client was written for is a loopback Ollama, so a stub
//! server on `127.0.0.1` exercises more of the truth than a differential
//! against the retired crate would: these tests assert the exact BYTES that
//! reach a server and the exact body that comes back, through real TCP, real
//! timeouts and the real authority guard — not that two libraries agree about
//! an abstraction.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use aterm_http::{Client, Error, Guard, ProxyMode, Trust};

/// Read a request head (to the blank line) plus `Content-Length` body bytes.
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

/// A one-shot stub server. Returns its endpoint and a channel carrying the
/// request it observed.
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
    Client::new(
        Trust::PlatformVerifier,
        ProxyMode::Direct,
        Duration::from_secs(10),
    )
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

    let (head, sent) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // The request line is origin-form and the Host carries the explicit port.
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
fn a_chunked_reply_is_reassembled() {
    // Ollama replies chunked when it does not know the length up front.
    let (endpoint, _rx) = stub(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n9\r\n{\"a\":\"bc\"\r\n2\r\n}\n\r\n0\r\n\r\n",
    );
    let response = client().post(&endpoint).limit(4096).send(b"{}").unwrap();
    assert_eq!(response.body(), b"{\"a\":\"bc\"}\n");
}

#[test]
fn a_chunk_size_at_the_top_of_usize_is_an_error_not_a_panic() {
    // The exact byte sequence that reproduced the defect: one real chunk, then
    // a chunk-size line of ffffffffffffffff (usize::MAX). The old `body.len() +
    // size > limit` panicked outright under debug-assertions and WRAPPED in
    // release — where the wrap passed the limit check, resize truncated instead
    // of grew, and read_exact panicked on the slice range. A panic here unwinds
    // the single named worker thread, which has no catch_unwind, so smart
    // titles would stay dead for the rest of the process's life.
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

#[test]
fn conflicting_duplicate_content_lengths_are_refused() {
    // One `find` returning the first of N is how two peers end up disagreeing
    // about where this response ends. RFC 9112 also permits only 1*DIGIT, so a
    // sign-prefixed length — which Rust's usize parser accepts — is refused too.
    for response in [
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 9\r\n\r\nhi" as &[u8],
        b"HTTP/1.1 200 OK\r\nContent-Length: +2\r\n\r\nhi",
    ] {
        let (endpoint, _rx) = stub(response);
        let error = client()
            .post(&endpoint)
            .limit(4096)
            .send(b"{}")
            .expect_err("ambiguous framing must be refused");
        assert!(matches!(error, Error::Protocol(_)), "{error:?}");
    }
}

#[test]
fn a_non_2xx_status_reaches_the_caller_intact() {
    let (endpoint, _rx) = stub(b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nnot here\n");
    let response = client().post(&endpoint).limit(4096).send(b"{}").unwrap();
    assert_eq!(response.status(), 404);
    assert!(!response.is_success());
    assert_eq!(response.body(), b"not here\n");
}

#[test]
fn an_oversized_body_errors_rather_than_truncating() {
    let (endpoint, _rx) = stub(
        b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\n\r\n0123456789012345678901234567890123456789012345678901234567890123",
    );
    let error = client()
        .post(&endpoint)
        .limit(16)
        .send(b"{}")
        .expect_err("over the limit");
    assert!(matches!(error, Error::TooLarge { limit: 16 }), "{error:?}");
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
    // The UI thread can revoke while the worker is mid-request. Terminal
    // context must not reach the socket after that.
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

    let seen = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
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
    // Accept and then never reply.
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            std::thread::sleep(Duration::from_secs(10));
            drop(stream);
        }
    });
    let client = Client::new(
        Trust::PlatformVerifier,
        ProxyMode::Direct,
        Duration::from_millis(250),
    );
    let started = std::time::Instant::now();
    let error = client
        .post(&format!("http://127.0.0.1:{port}/api/chat"))
        .limit(4096)
        .send(b"{}")
        .expect_err("a silent server must time out");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
    let _ = error;
}

/// A stub that captures every byte it is sent, to EOF, and never replies.
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
    // The trust model is this crate's headline claim, and the regression that
    // would hurt most is the silent one: `Client::open` stops running the
    // handshake and the worker's prompt — terminal context, and a bearer token
    // in the headers — goes out in the clear. This peer speaks no TLS at all,
    // so the handshake cannot finish; what is asserted is what reached the
    // socket BEFORE it failed.
    //
    // Non-vacuity is the 0x16 check: a downgrade to plaintext would put "POST "
    // in this buffer instead of a TLS handshake record, and the canary assert
    // below would fire on the same run.
    //
    // NOT covered here, deliberately: that a server certificate is actually
    // VERIFIED, and an invalid one refused. That needs a peer holding a real
    // certificate, which needs a private key — and a key fixture cannot be
    // tracked (tools/grep_guard.sh B6/B8), while generating one at test time
    // means either an `openssl s_server` child (LibreSSL here, OpenSSL on CI —
    // different flags, a bound port, and a process to reap) or hand-rolled
    // X.509 DER against a new dev-dependency. In a change whose whole purpose
    // is retiring dependencies, neither is the right trade to make silently.
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
        .recv_timeout(Duration::from_secs(10))
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

/// A peer that completes the TCP connect, claims a big TLS record, and then
/// emits one byte at a time forever. It never speaks TLS; the point is only
/// that bytes keep ARRIVING.
fn dribbling_tls_peer(step: Duration, steps: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        // A handshake record header claiming 16384 bytes of body, so rustls
        // stays in `read_tls` waiting for a flight that never completes.
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
    // The deadline has to be enforced INSIDE the handshake, not just before it:
    // `complete_io` loops on `read_tls`, so a per-syscall timeout applied once
    // at the top bounds one read while every dribbled byte restarts the clock.
    // This peer dribbles for ~6s against a client budgeted 1s. Before the fix
    // the client stayed on the socket until the stub gave up; the bound below
    // is what makes this test non-vacuous rather than the error alone.
    let endpoint = dribbling_tls_peer(Duration::from_millis(100), 60);
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
        elapsed < Duration::from_secs(3),
        "the handshake ran {elapsed:?} on a 1s budget: {error:?}"
    );
    assert!(matches!(error, Error::Io(_)), "{error:?}");
}

#[test]
fn a_refused_port_is_an_error_not_a_panic() {
    // Bind then drop, so the port is almost certainly closed.
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
        // The refusal must not echo what it refused. This message reaches
        // stderr through the title-summary worker, and the userinfo shape above
        // is refused precisely because it carries a credential; printing the
        // raw endpoint back would put that password on the terminal.
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(
                !rendered.contains("pass"),
                "{bad}: the error echoed the credential: {rendered}"
            );
        }
    }
}
