// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Wire tests for HTTP methods, `Content-Length`, `Accept`, bodiless responses, and headers.

use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;

use aterm_http::proxy::EnvSource;
use aterm_http::{Body, Client, Error, Method, Phase, ProxyMode, Trust};

const HANG_DETECTOR: Duration = Duration::from_secs(60);

struct NoEnv;

impl EnvSource for NoEnv {
    fn var_os(&self, _name: &str) -> Option<OsString> {
        None
    }
}

fn client() -> Client {
    Client::new(Trust::Roots(Vec::new()), ProxyMode::Direct, HANG_DETECTOR)
}

fn read_request(stream: &mut TcpStream) -> Vec<u8> {
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
    if stream.read_exact(&mut body).is_ok() {
        seen.extend_from_slice(&body);
    }
    seen
}

fn serve(
    response: &[u8],
    connections: usize,
    hold: bool,
) -> (u16, mpsc::Receiver<Vec<u8>>, mpsc::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let response = response.to_vec();
    std::thread::spawn(move || {
        for _ in 0..connections {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = stream.set_read_timeout(Some(HANG_DETECTOR));
            let seen = read_request(&mut stream);
            let _ = stream.write_all(&response);
            let _ = stream.flush();
            let _ = tx.send(seen);
            if hold {
                let _ = released.recv_timeout(HANG_DETECTOR);
            }
        }
    });
    (port, rx, release)
}

fn lower_head(seen: &[u8]) -> String {
    let text = String::from_utf8_lossy(seen).to_ascii_lowercase();
    text.split("\r\n\r\n").next().unwrap_or_default().to_owned()
}

fn url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/m")
}

fn recorded(method: Method, body: Body<'_>) -> Vec<u8> {
    let (port, rx, _release) = serve(b"HTTP/1.1 204 No Content\r\n\r\n", 1, false);
    let exchange = client()
        .request(method, &url(port))
        .exchange_with_env(body, &NoEnv)
        .expect("the listener answers 204");
    assert_eq!(exchange.status(), 204);
    rx.recv_timeout(HANG_DETECTOR).unwrap()
}

#[test]
fn get_and_head_carry_no_content_length() {
    for method in [Method::Get, Method::Head] {
        let seen = recorded(method, Body::Empty);
        let head = lower_head(&seen);
        assert!(
            head.starts_with(&format!(
                "{} /m http/1.1\r\n",
                method.as_str().to_ascii_lowercase()
            )),
            "{head}"
        );
        assert!(!head.contains("content-length"), "{method:?}: {head}");
    }
}

#[test]
fn a_head_answer_that_declares_a_length_reads_as_an_empty_body_at_once() {
    let (port, _rx, release) = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n", 1, true);
    let exchange = client()
        .request(Method::Head, &url(port))
        .exchange_with_env(Body::Empty, &NoEnv)
        .expect("the listener answers");
    assert_eq!(exchange.status(), 200);
    assert_eq!(exchange.header("content-length"), Some("10"));
    let mut body = exchange.into_body();
    assert_eq!(body.declared_len(), Some(0));
    let mut bytes = Vec::new();
    body.read_to_end(&mut bytes).unwrap();
    assert!(bytes.is_empty());
    assert_eq!(body.consumed(), 0);
    drop(release);
}

#[test]
fn delete_carries_a_length_only_with_a_body() {
    let empty = lower_head(&recorded(Method::Delete, Body::Empty));
    assert!(!empty.contains("content-length"), "{empty}");

    let seen = recorded(Method::Delete, Body::Bytes(b"abc"));
    assert!(lower_head(&seen).contains("\r\ncontent-length: 3\r\n"));
    assert!(seen.ends_with(b"\r\n\r\nabc"));
}

#[test]
fn post_patch_and_put_carry_a_zero_length_when_empty() {
    for method in [Method::Post, Method::Patch, Method::Put] {
        let head = lower_head(&recorded(method, Body::Empty));
        assert!(
            head.contains("\r\ncontent-length: 0\r\n"),
            "{method:?}: {head}"
        );
    }
}

#[test]
fn a_body_on_a_get_or_head_is_refused_before_any_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    for method in [Method::Get, Method::Head] {
        let failed = client()
            .request(method, &url(port))
            .exchange_with_env(Body::Bytes(b"x"), &NoEnv)
            .expect_err("a GET or HEAD carries no body");
        assert_eq!(failed.phase, Phase::NotSent, "{method:?}");
        assert!(
            matches!(&failed.error, Error::Invalid(message) if message.contains(method.as_str())),
            "{failed:?}"
        );
        let mut reader: &[u8] = b"yz";
        let failed = client()
            .request(method, &url(port))
            .exchange_with_env(
                Body::Reader {
                    len: 2,
                    reader: &mut reader,
                },
                &NoEnv,
            )
            .expect_err("a streamed body is a body too");
        assert_eq!(failed.phase, Phase::NotSent);
        assert!(matches!(failed.error, Error::Invalid(_)), "{failed:?}");
    }
    listener.set_nonblocking(true).unwrap();
    match listener.accept() {
        Err(error) if error.kind() == ErrorKind::WouldBlock => {}
        other => panic!("a refused request still connected: {other:?}"),
    }
}

#[test]
fn the_caller_owns_accept() {
    let (port, rx, _release) = serve(b"HTTP/1.1 204 No Content\r\n\r\n", 2, false);
    client()
        .request(Method::Get, &url(port))
        .header("accept", "application/vnd.github+json")
        .exchange_with_env(Body::Empty, &NoEnv)
        .unwrap();
    let head = lower_head(&rx.recv_timeout(HANG_DETECTOR).unwrap());
    assert_eq!(head.matches("\r\naccept:").count(), 1, "{head}");
    assert!(
        head.lines()
            .any(|line| line == "accept: application/vnd.github+json"),
        "{head}"
    );

    client()
        .request(Method::Get, &url(port))
        .exchange_with_env(Body::Empty, &NoEnv)
        .unwrap();
    let head = lower_head(&rx.recv_timeout(HANG_DETECTOR).unwrap());
    assert_eq!(head.matches("\r\naccept:").count(), 1, "{head}");
    assert!(head.lines().any(|line| line == "accept: */*"), "{head}");
}

const LINKED: &[u8] = b"HTTP/1.1 200 OK\r\nLink: <https://a.test/?page=2>; rel=\"next\"\r\nX-B: 2\r\nlink: <https://a.test/?page=9>; rel=\"last\"\r\nContent-Length: 0\r\n\r\n";

#[test]
fn headers_keep_their_order_and_repeated_ones_are_all_returned() {
    let (port, _rx, _release) = serve(LINKED, 2, false);
    let expected = vec![
        ("Link", "<https://a.test/?page=2>; rel=\"next\""),
        ("X-B", "2"),
        ("link", "<https://a.test/?page=9>; rel=\"last\""),
        ("Content-Length", "0"),
    ];
    let links = vec![
        "<https://a.test/?page=2>; rel=\"next\"",
        "<https://a.test/?page=9>; rel=\"last\"",
    ];

    let exchange = client()
        .request(Method::Get, &url(port))
        .exchange_with_env(Body::Empty, &NoEnv)
        .unwrap();
    assert_eq!(exchange.headers().collect::<Vec<_>>(), expected);
    assert_eq!(exchange.header_all("LINK").collect::<Vec<_>>(), links);
    assert_eq!(exchange.header("link"), Some(links[0]));
    assert_eq!(exchange.header_all("x-missing").count(), 0);

    let response = client()
        .request(Method::Get, &url(port))
        .send_with_env(b"", &NoEnv)
        .unwrap();
    assert_eq!(response.headers().collect::<Vec<_>>(), expected);
    assert_eq!(response.header_all("Link").collect::<Vec<_>>(), links);
}

#[test]
fn no_content_and_not_modified_read_as_empty_whatever_they_declare() {
    for answer in [
        &b"HTTP/1.1 204 No Content\r\nContent-Length: 5\r\n\r\n"[..],
        &b"HTTP/1.1 304 Not Modified\r\nContent-Length: 5\r\n\r\n"[..],
    ] {
        let (port, _rx, release) = serve(answer, 1, true);
        let exchange = client()
            .request(Method::Get, &url(port))
            .exchange_with_env(Body::Empty, &NoEnv)
            .unwrap();
        let mut bytes = Vec::new();
        let mut body = exchange.into_body();
        body.read_to_end(&mut bytes).unwrap();
        assert!(bytes.is_empty());
        assert_eq!(body.declared_len(), Some(0));
        drop(release);
    }
}

#[test]
fn request_post_writes_the_same_bytes_as_post() {
    let (port, rx, _release) = serve(b"HTTP/1.1 204 No Content\r\n\r\n", 2, false);
    client()
        .post(&url(port))
        .header("X-A", "1")
        .send_with_env(b"hello", &NoEnv)
        .unwrap();
    let by_post = rx.recv_timeout(HANG_DETECTOR).unwrap();
    client()
        .request(Method::Post, &url(port))
        .header("X-A", "1")
        .send_with_env(b"hello", &NoEnv)
        .unwrap();
    let by_request = rx.recv_timeout(HANG_DETECTOR).unwrap();
    assert_eq!(by_post, by_request);
    assert!(by_post.starts_with(b"POST /m HTTP/1.1\r\n"));
}
