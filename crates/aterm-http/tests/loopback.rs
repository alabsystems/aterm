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
    }
}
