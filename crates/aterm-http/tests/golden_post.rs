// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! THE BYTES `post()` PUTS ON THE WIRE, pinned exactly.
//!
//! Written and passing before the method, streaming and phase refactor (design §5): every
//! change to the request path is additive, so `post().send_with_env()` must keep writing
//! these bytes, header order included. A loopback listener records what arrives.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

use aterm_http::proxy::EnvSource;
use aterm_http::{Client, ProxyMode, Trust};

/// A wait for something that must happen is a hang detector, never a latency budget.
const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// An environment with no variable set: no proxy, whatever the process has.
struct NoEnv;

impl EnvSource for NoEnv {
    fn var_os(&self, _name: &str) -> Option<OsString> {
        None
    }
}

/// Read one request: the head to its blank line, then `Content-Length` body bytes.
fn read_request(stream: &mut impl Read) -> Vec<u8> {
    let mut seen = Vec::new();
    let mut byte = [0u8; 1];
    while !seen.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).unwrap_or(0) == 0 {
            return seen;
        }
        seen.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&seen).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).unwrap();
    seen.extend_from_slice(&body);
    seen
}

/// A one-shot listener: it records the request and answers `204`, then records
/// whatever else arrives until the client closes.
fn recording_listener() -> (u16, mpsc::Receiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(HANG_DETECTOR)).unwrap();
        let mut seen = read_request(&mut stream);
        let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\n\r\n");
        // Everything else the client writes before it closes is recorded
        // too, so a byte past the declared body cannot pass unseen.
        let _ = stream.read_to_end(&mut seen);
        let _ = tx.send(seen);
    });
    (port, rx)
}

fn golden(port: u16) -> String {
    format!(
        "POST /v1/x HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: 5\r\nAccept: */*\r\nX-A: 1\r\n\r\nhello"
    )
}

#[test]
fn post_writes_the_golden_bytes() {
    let (port, rx) = recording_listener();
    let response = Client::new(
        Trust::Roots(Vec::new()),
        ProxyMode::Direct,
        Duration::from_secs(10),
    )
    .post(&format!("http://127.0.0.1:{port}/v1/x"))
    .header("X-A", "1")
    .send_with_env(b"hello", &NoEnv)
    .expect("the listener answers 204");
    assert_eq!(response.status(), 204);
    let seen = rx.recv_timeout(HANG_DETECTOR).unwrap();
    assert_eq!(String::from_utf8_lossy(&seen), golden(port));
}

#[test]
fn caller_framing_headers_are_dropped_from_the_golden_bytes() {
    let (port, rx) = recording_listener();
    Client::new(
        Trust::Roots(Vec::new()),
        ProxyMode::Direct,
        Duration::from_secs(10),
    )
    .post(&format!("http://127.0.0.1:{port}/v1/x"))
    .header("Host", "evil.test")
    .header("X-A", "1")
    .header("Content-Length", "99")
    .header("Connection", "keep-alive")
    .header("Transfer-Encoding", "chunked")
    .send_with_env(b"hello", &NoEnv)
    .expect("the listener answers 204");
    let seen = rx.recv_timeout(HANG_DETECTOR).unwrap();
    assert_eq!(String::from_utf8_lossy(&seen), golden(port));
}
