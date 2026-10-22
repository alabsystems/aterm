// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! STREAMED BODIES, PHASES AND DEADLINES over loopback sockets (design §5.2).
//!
//! An upload streams from a reader of a stated length, and a download is read
//! incrementally under the response's framing and the request's limit. A
//! failure says how far the request got (`Phase`), and a quiet peer is named
//! by the bound that caught it: the idle cap, the total, or the connect
//! budget. Each time-based case asserts the error and its text, never the time
//! taken; the client's own deadline is the 60 s hang detector.

use std::ffi::OsString;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use aterm_http::proxy::EnvSource;
use aterm_http::{
    Body, Client, Connect, Deadline, Error, Method, Phase, ProxyMode, SendError, Trust,
};
use aterm_uds::secret::Secret;

/// A wait for something that must happen is a hang detector, never a latency budget.
const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// One mebibyte.
const MIB: usize = 1024 * 1024;

/// An environment with no variable set: no proxy, whatever the process has.
struct NoEnv;

impl EnvSource for NoEnv {
    fn var_os(&self, _name: &str) -> Option<OsString> {
        None
    }
}

fn client() -> Client {
    Client::new(Trust::Roots(Vec::new()), ProxyMode::Direct, HANG_DETECTOR)
}

/// `n` bytes of a pattern no run of zeros could fake.
fn pattern(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| u8::try_from(i % 251).unwrap_or_default())
        .collect()
}

/// Read one request's head, to its blank line.
fn read_head(stream: &mut TcpStream) -> Vec<u8> {
    let mut seen = Vec::new();
    let mut byte = [0u8; 1];
    while !seen.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).unwrap_or(0) == 0 {
            break;
        }
        seen.push(byte[0]);
    }
    seen
}

/// The `Content-Length` a recorded head declares.
fn declared(head: &[u8]) -> usize {
    String::from_utf8_lossy(head)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0)
}

/// A listener for one connection, handled by `handle` on its own thread,
/// whose result comes back on the receiver.
fn one_connection<T: Send + 'static>(
    handle: impl FnOnce(TcpStream) -> T + Send + 'static,
) -> (String, mpsc::Receiver<T>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let _ = stream.set_read_timeout(Some(HANG_DETECTOR));
        let _ = tx.send(handle(stream));
    });
    (format!("http://127.0.0.1:{port}/s"), rx)
}

/// A listener that reads the request head and answers with `answer`, then
/// closes.
fn answering(answer: Vec<u8>) -> String {
    one_connection(move |mut stream| {
        let _ = read_head(&mut stream);
        let _ = stream.write_all(&answer);
    })
    .0
}

/// GET `endpoint` and read its whole body through the [`aterm_http::BodyReader`].
fn read_body(endpoint: &str, limit: usize) -> io::Result<Vec<u8>> {
    let exchange = client()
        .request(Method::Get, endpoint)
        .limit(limit)
        .exchange_with_env(Body::Empty, &NoEnv)
        .expect("the listener answers");
    let mut bytes = Vec::new();
    exchange.into_body().read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The loopback address an [`one_connection`] endpoint names.
fn loopback_addr(endpoint: &str) -> SocketAddr {
    let port = endpoint
        .trim_start_matches("http://127.0.0.1:")
        .trim_end_matches("/s")
        .parse::<u16>()
        .unwrap();
    SocketAddr::from(([127, 0, 0, 1], port))
}

fn io_error(failed: &SendError) -> &io::Error {
    match &failed.error {
        Error::Io(error) => error,
        other => panic!("not an I/O failure: {other:?}"),
    }
}

#[test]
fn a_streamed_upload_arrives_whole_with_its_length() {
    let data = pattern(300_000);
    let (endpoint, rx) = one_connection(|mut stream| {
        let head = read_head(&mut stream);
        let mut body = vec![0u8; declared(&head)];
        stream.read_exact(&mut body).unwrap();
        let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\n\r\n");
        (head, body)
    });
    let mut reader: &[u8] = &data;
    let exchange = client()
        .request(Method::Put, &endpoint)
        .exchange_with_env(
            Body::Reader {
                len: 300_000,
                reader: &mut reader,
            },
            &NoEnv,
        )
        .expect("the listener answers 204");
    assert_eq!(exchange.status(), 204);
    let (head, body) = rx.recv_timeout(HANG_DETECTOR).unwrap();
    assert!(
        String::from_utf8_lossy(&head).contains("\r\nContent-Length: 300000\r\n"),
        "{}",
        String::from_utf8_lossy(&head)
    );
    assert_eq!(body, data);
}

#[test]
fn a_reader_shorter_than_its_length_fails_after_the_head_was_sent() {
    let (endpoint, _rx) = one_connection(|mut stream| {
        let _ = read_head(&mut stream);
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest);
    });
    let mut reader: &[u8] = b"four";
    let failed = client()
        .request(Method::Put, &endpoint)
        .exchange_with_env(
            Body::Reader {
                len: 10,
                reader: &mut reader,
            },
            &NoEnv,
        )
        .expect_err("the reader ends six bytes early");
    assert_eq!(failed.phase, Phase::Sent);
    let error = io_error(&failed);
    assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
    // The counts it carries: where the reader ended, and the length promised.
    let text = error.to_string();
    assert!(text.contains(" 4 ") && text.contains(" 10 "), "{text}");
}

/// A body reader whose own read fails `kind`.
struct Failing(ErrorKind);

impl Read for Failing {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(self.0, "the local file went away"))
    }
}

#[test]
fn a_failing_body_reader_is_never_read_as_the_socket_failing() {
    // `PermissionDenied` from a socket is a revoked authority; from the
    // caller's own reader it is a local failure, and must not read as one.
    for kind in [ErrorKind::PermissionDenied, ErrorKind::TimedOut] {
        let (endpoint, _rx) = one_connection(|mut stream| {
            let _ = read_head(&mut stream);
            let mut rest = Vec::new();
            let _ = stream.read_to_end(&mut rest);
        });
        let mut reader = Failing(kind);
        let failed = client()
            .request(Method::Put, &endpoint)
            .exchange_with_env(
                Body::Reader {
                    len: 10,
                    reader: &mut reader,
                },
                &NoEnv,
            )
            .expect_err("the reader fails");
        assert_eq!(failed.phase, Phase::Sent, "{failed:?}");
        assert!(!failed.error.is_revoked(), "{failed:?}");
        let error = io_error(&failed);
        assert_eq!(error.kind(), ErrorKind::Other, "{error}");
        let text = error.to_string();
        assert!(!text.starts_with("no bytes for"), "{text}");
        assert!(text.contains(" 0 ") && text.contains(" 10 "), "{text}");
    }
}

#[test]
fn a_mebibyte_answer_reads_incrementally() {
    let data = pattern(MIB);
    let mut answer = format!("HTTP/1.1 200 OK\r\nContent-Length: {MIB}\r\n\r\n").into_bytes();
    answer.extend_from_slice(&data);
    let endpoint = answering(answer);
    let exchange = client()
        .request(Method::Get, &endpoint)
        .exchange_with_env(Body::Empty, &NoEnv)
        .unwrap();
    let mut body = exchange.into_body();
    assert_eq!(body.declared_len(), Some(1_048_576));
    let mut read = Vec::with_capacity(MIB);
    let mut buffer = [0u8; 4096];
    loop {
        let n = body.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        assert!(n <= buffer.len());
        read.extend_from_slice(&buffer[..n]);
        assert_eq!(body.consumed(), u64::try_from(read.len()).unwrap());
    }
    assert_eq!(body.consumed(), 1_048_576);
    assert_eq!(read, data);
}

#[test]
fn a_chunked_mebibyte_in_small_chunks_reads_whole() {
    // 16,384 chunks of 64 bytes: their framing lines total far more than one
    // header block's budget, which bounds each line, not the whole body.
    let data = pattern(MIB);
    let mut answer = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for chunk in data.chunks(64) {
        answer.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        answer.extend_from_slice(chunk);
        answer.extend_from_slice(b"\r\n");
    }
    answer.extend_from_slice(b"0\r\n\r\n");
    let endpoint = answering(answer);
    let exchange = client()
        .request(Method::Get, &endpoint)
        .exchange_with_env(Body::Empty, &NoEnv)
        .unwrap();
    let mut body = exchange.into_body();
    assert_eq!(body.declared_len(), None);
    let mut read = Vec::new();
    body.read_to_end(&mut read).unwrap();
    assert_eq!(body.consumed(), 1_048_576);
    assert_eq!(read, data);
}

#[test]
fn a_body_cut_short_is_unexpected_eof() {
    let endpoint = answering(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhalf.".to_vec());
    let error = read_body(&endpoint, usize::MAX).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UnexpectedEof, "{error}");

    let endpoint =
        answering(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel".to_vec());
    let error = read_body(&endpoint, usize::MAX).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UnexpectedEof, "{error}");
}

#[test]
fn bad_chunk_framing_is_invalid_data() {
    for answer in [
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nhello\r\n0\r\n\r\n"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhiXX0\r\n\r\n"[..],
    ] {
        let endpoint = answering(answer.to_vec());
        let error = read_body(&endpoint, usize::MAX).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData, "{error}");
        let inner = error.get_ref().and_then(|e| e.downcast_ref::<Error>());
        assert!(matches!(inner, Some(Error::Protocol(_))), "{error:?}");
    }
}

#[test]
fn a_body_past_the_limit_is_too_large_inside_the_io_error() {
    for answer in [
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n".to_vec(),
        [&b"HTTP/1.1 200 OK\r\n\r\n"[..], &pattern(100)].concat(),
        [
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n64\r\n"[..],
            &pattern(100),
            b"\r\n0\r\n\r\n",
        ]
        .concat(),
    ] {
        let endpoint = answering(answer);
        let error = read_body(&endpoint, 16).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Other, "{error}");
        let inner = error.get_ref().and_then(|e| e.downcast_ref::<Error>());
        assert!(
            matches!(inner, Some(Error::TooLarge { limit: 16 })),
            "{error:?}"
        );
    }
}

#[test]
fn a_refused_connect_was_not_sent() {
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let failed = client()
        .request(Method::Get, &format!("http://127.0.0.1:{port}/s"))
        .exchange_with_env(Body::Empty, &NoEnv)
        .expect_err("nothing listens");
    assert_eq!(failed.phase, Phase::NotSent, "{failed:?}");
    assert!(!failed.error.is_revoked());
}

#[test]
fn a_server_that_reads_the_request_and_closes_leaves_it_sent() {
    let (endpoint, _rx) = one_connection(|mut stream| {
        let _ = read_head(&mut stream);
    });
    let failed = client()
        .request(Method::Get, &endpoint)
        .exchange_with_env(Body::Empty, &NoEnv)
        .expect_err("the server answers nothing");
    assert_eq!(failed.phase, Phase::Sent, "{failed:?}");
    assert_eq!(io_error(&failed).kind(), ErrorKind::UnexpectedEof);
}

#[test]
fn a_server_that_closes_mid_body_had_answered() {
    let endpoint =
        answering(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nX-Seen: yes\r\n\r\nshort".to_vec());
    // The exchange reached Answered: its head parsed, and only the body's
    // reader meets the cut.
    let exchange = client()
        .request(Method::Get, &endpoint)
        .exchange_with_env(Body::Empty, &NoEnv)
        .expect("the head parses");
    assert_eq!(exchange.status(), 200);
    assert_eq!(exchange.header("x-seen"), Some("yes"));
    let mut bytes = Vec::new();
    let error = exchange.into_body().read_to_end(&mut bytes).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
}

/// A server that refuses an upload after its head (RFC 9112 §9.5) answers
/// and closes over the body it never read, so the client's next write fails;
/// the answer, already received, is what the exchange returns. Unix only:
/// Windows discards the bytes a socket received once a reset arrives.
#[cfg(unix)]
#[test]
fn an_answer_sent_while_the_body_was_still_going_out_is_read() {
    let (endpoint, _rx) = one_connection(|mut stream| {
        let _ = read_head(&mut stream);
        // The answer leaves at once, never held back by Nagle for the close's
        // reset to discard.
        let _ = stream.set_nodelay(true);
        let _ = stream
            .write_all(b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 9\r\n\r\ntoo large");
        // Dropped over the unread body: the client's next write meets the reset.
    });
    // Far more than the socket buffers hold, so the server's close lands
    // while the body is still being written.
    let len = 64 * MIB;
    let mut zeros = io::repeat(0).take(u64::try_from(len).unwrap());
    let exchange = client()
        .request(Method::Put, &endpoint)
        .exchange_with_env(
            Body::Reader {
                len: u64::try_from(len).unwrap(),
                reader: &mut zeros,
            },
            &NoEnv,
        )
        .expect("the early answer is the exchange's");
    assert_eq!(exchange.status(), 413);
    let mut bytes = Vec::new();
    exchange.into_body().read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"too large");
}

#[test]
fn a_head_whose_framing_is_refused_had_answered() {
    let endpoint = answering(
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\nhello".to_vec(),
    );
    let failed = client()
        .request(Method::Get, &endpoint)
        .exchange_with_env(Body::Empty, &NoEnv)
        .expect_err("a body boundary two parties could disagree on is refused");
    assert_eq!(failed.phase, Phase::Answered, "{failed:?}");
    assert!(matches!(failed.error, Error::Protocol(_)), "{failed:?}");
}

#[test]
fn an_unrequested_switch_of_protocols_had_answered() {
    // A 101 is no interim head here (nothing asked to upgrade): its status
    // line and headers parsed, so it is the final head, and refused at Answered.
    for method in [Method::Get, Method::Head] {
        let endpoint = answering(
            b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: h2c\r\nConnection: Upgrade\r\n\r\n"
                .to_vec(),
        );
        let failed = client()
            .request(method, &endpoint)
            .exchange_with_env(Body::Empty, &NoEnv)
            .expect_err("an upgrade nobody asked for is refused");
        assert_eq!(failed.phase, Phase::Answered, "{method:?}: {failed:?}");
        assert!(
            matches!(&failed.error, Error::Protocol(text) if text.contains("101")),
            "{method:?}: {failed:?}"
        );
    }
}

/// An environment that names one `HTTPS_PROXY` and nothing else.
struct HttpsProxyEnv(String);

impl EnvSource for HttpsProxyEnv {
    fn var_os(&self, name: &str) -> Option<OsString> {
        (name == "HTTPS_PROXY").then(|| self.0.clone().into())
    }
}

#[test]
fn a_proxy_that_refuses_the_tunnel_is_a_protocol_error_unsent() {
    let (proxy, rx) = one_connection(|mut stream| {
        let head = read_head(&mut stream);
        let _ = stream.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n");
        head
    });
    let proxy = proxy.trim_end_matches("/s").to_owned();
    let failed = Client::new(
        Trust::Roots(Vec::new()),
        ProxyMode::Environment,
        HANG_DETECTOR,
    )
    .request(Method::Get, "https://origin.test/s")
    .exchange_with_env(Body::Empty, &HttpsProxyEnv(proxy))
    .expect_err("the proxy refuses the tunnel");
    let head = String::from_utf8_lossy(&rx.recv_timeout(HANG_DETECTOR).unwrap()).into_owned();
    assert!(head.starts_with("CONNECT origin.test:443 "), "{head}");
    assert_eq!(failed.phase, Phase::NotSent, "{failed:?}");
    assert!(
        matches!(&failed.error, Error::Protocol(text) if text.starts_with("proxy ")),
        "{failed:?}"
    );
}

#[test]
fn the_request_deadline_takes_over_once_the_connect_budget_has_opened_the_stream() {
    // The answer comes later than the whole connect budget could last, so it
    // is read only if the stream left that budget behind when it opened.
    const CONNECT: Duration = Duration::from_secs(2);
    let (endpoint, _rx) = one_connection(|mut stream| {
        let _ = read_head(&mut stream);
        std::thread::sleep(CONNECT + Duration::from_millis(200));
        let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\n\r\n");
    });
    let exchange = client()
        .request(Method::Get, &endpoint)
        .connect_within(CONNECT)
        .exchange_with_env(Body::Empty, &NoEnv)
        .expect("the request's own deadline, not the connect budget, bounds the answer");
    assert_eq!(exchange.status(), 204);
}

#[test]
fn exchange_and_body_reader_are_send() {
    fn assert_send<T: Send + 'static>() {}
    assert_send::<aterm_http::Exchange>();
    assert_send::<aterm_http::BodyReader>();
}

#[test]
fn a_silent_server_hits_the_idle_cap() {
    // Never accepted: the kernel completes the connect and takes the request
    // into its buffer, and no byte ever comes back.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let failed = client()
        .request(Method::Get, &format!("http://127.0.0.1:{port}/s"))
        .idle(Duration::from_millis(200))
        .exchange_with_env(Body::Empty, &NoEnv)
        .expect_err("a silent server must time out");
    assert_eq!(failed.phase, Phase::Sent, "{failed:?}");
    let error = io_error(&failed);
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert_eq!(error.to_string(), "no bytes for 200ms");
    drop(listener);
}

#[test]
fn a_trickling_server_hits_the_total_deadline() {
    // One byte every 100 ms keeps any idle cap satisfied; only the total ends it.
    // The status line and the start of one header go once, then the header's
    // value trickles: however many bytes a stalled read collects, the head is
    // one valid, unfinished header line (600 bytes, under the line cap), so no
    // parse error can end the exchange before the deadline does.
    let (endpoint, _rx) = one_connection(|mut stream| {
        let _ = read_head(&mut stream);
        if stream.write_all(b"HTTP/1.1 200 OK\r\nX-Trickle: ").is_err() || stream.flush().is_err() {
            return;
        }
        for _ in 0..600 {
            if stream.write_all(b"a").is_err() || stream.flush().is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    // Dialled straight, so no DNS wait shares the 300 ms with the deadline
    // under test.
    let failed = client()
        .with_connector(Arc::new(Straight(loopback_addr(&endpoint))))
        .request(Method::Get, &endpoint)
        .idle(HANG_DETECTOR)
        .backstop(Duration::from_millis(300))
        .exchange_with_env(Body::Empty, &NoEnv)
        .expect_err("the trickle outlives the backstop");
    // No phase is asserted: a stall before the head's first byte would leave
    // it NotSent under this short a total. Sent is pinned by the cases above.
    // The data its text carries: the request's own deadline fired, not the
    // idle cap (whose `no bytes for` agh classifies by) nor the connect budget.
    let error = io_error(&failed);
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    let text = error.to_string();
    assert!(!text.starts_with("no bytes for"), "{text}");
    assert!(
        text.contains("request") && text.contains("deadline") && !text.contains("connect"),
        "{text}"
    );
}

/// A connector that dials one address and resolves nothing, so no DNS wait
/// shares the connect budget with the handshake.
#[derive(Debug)]
struct Straight(SocketAddr);

impl Connect for Straight {
    fn connect(&self, _: &str, _: u16, _: Deadline) -> io::Result<TcpStream> {
        TcpStream::connect(self.0)
    }
}

#[test]
fn a_tls_handshake_that_never_answers_hits_the_connect_budget_unsent() {
    // A plain listener that never accepts: the ClientHello lands in the
    // kernel's buffer and no ServerHello ever comes.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // The suite's own root, so the host's trust store plays no part: no
    // certificate is ever checked, since the peer never answers.
    let roots = vec![include_bytes!("testdata/loopback-cert.der").to_vec()];
    let failed = Client::new(Trust::Roots(roots), ProxyMode::Direct, HANG_DETECTOR)
        .with_connector(Arc::new(Straight(listener.local_addr().unwrap())))
        .request(Method::Get, &format!("https://127.0.0.1:{port}/s"))
        .connect_within(Duration::from_millis(200))
        .exchange_with_env(Body::Empty, &NoEnv)
        .expect_err("the handshake cannot finish");
    assert_eq!(failed.phase, Phase::NotSent, "{failed:?}");
    // The data its text carries: the connect budget fired, not the 60 s total
    // (which would also end NotSent and TimedOut, a minute later).
    let error = io_error(&failed);
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    let text = error.to_string();
    assert!(!text.starts_with("no bytes for"), "{text}");
    assert!(!text.starts_with("dns:"), "{text}");
    assert!(
        text.contains("connect") && text.contains("deadline") && !text.contains("request"),
        "{text}"
    );
    drop(listener);
}

#[test]
fn a_secret_header_is_on_the_wire_and_in_no_error_or_debug() {
    // Built at run time: no token-shaped literal is in the source.
    let value = format!("Bearer canary-{:x}", 0x00c0_ffee_u32);
    let (endpoint, rx) = one_connection(|mut stream| read_head(&mut stream));
    let client = client();
    let builder = client
        .request(Method::Get, &endpoint)
        .header("X-A", "1")
        .secret_header("Authorization", Secret::new(value.as_bytes()));
    let debug = format!("{builder:?}");
    assert!(!debug.contains(&value), "{debug}");
    assert!(!debug.contains("canary"), "{debug}");
    // The listener reads the head and closes, so the request fails after the
    // secret went out: the failure must not carry it either.
    let failed = builder
        .exchange_with_env(Body::Empty, &NoEnv)
        .expect_err("the listener answers nothing");
    let head = String::from_utf8_lossy(&rx.recv_timeout(HANG_DETECTOR).unwrap()).into_owned();
    assert!(
        head.ends_with(&format!("\r\nX-A: 1\r\nAuthorization: {value}\r\n\r\n")),
        "{head}"
    );
    for rendered in [
        failed.to_string(),
        format!("{failed:?}"),
        failed.error.to_string(),
    ] {
        assert!(!rendered.contains("canary"), "{rendered}");
    }
}

#[test]
fn an_exchange_debug_names_its_headers_without_their_values() {
    // Built at run time: no token-shaped literal is in the source.
    let signed = format!("canary-{:x}", 0x00c0_ffee_u32);
    let endpoint = answering(
        format!(
            "HTTP/1.1 302 Found\r\nLocation: /asset?signed={signed}\r\nContent-Length: 0\r\n\r\n"
        )
        .into_bytes(),
    );
    let exchange = client()
        .request(Method::Get, &endpoint)
        .exchange_with_env(Body::Empty, &NoEnv)
        .expect("the listener answers");
    assert_eq!(
        exchange.header("location"),
        Some(&*format!("/asset?signed={signed}"))
    );
    let debug = format!("{exchange:?}");
    assert!(debug.contains("Location"), "{debug}");
    assert!(!debug.contains("canary"), "{debug}");
}

#[test]
fn a_secret_header_with_a_reserved_name_is_refused_before_any_connect() {
    // A credential under a name the client generates would be dropped from
    // the head unsent, and a secret Accept would sit beside the default
    // `Accept: */*`; the builder says so instead, before any connection.
    for name in ["Proxy-Authorization", "Accept"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let failed = client()
            .request(Method::Get, &format!("http://127.0.0.1:{port}/s"))
            .secret_header(name, Secret::new(b"canary"))
            .exchange_with_env(Body::Empty, &NoEnv)
            .expect_err("a reserved name is refused");
        assert_eq!(failed.phase, Phase::NotSent, "{name}");
        assert!(
            matches!(&failed.error, Error::Invalid(text) if text.contains(name)),
            "{name}: {failed:?}"
        );
        assert!(!format!("{failed:?}").contains("canary"), "{name}");
        assert_eq!(
            listener.accept().map(|_| ()).map_err(|error| error.kind()),
            Err(ErrorKind::WouldBlock),
            "{name}: no connection was opened"
        );
    }
}

#[test]
fn a_secret_header_with_a_control_character_is_refused_without_its_value() {
    let failed = client()
        .request(Method::Get, "http://127.0.0.1:9/s")
        .secret_header("Authorization", Secret::new(b"canary\r\nX-Evil: 1"))
        .exchange_with_env(Body::Empty, &NoEnv)
        .expect_err("a control character is refused");
    assert_eq!(failed.phase, Phase::NotSent);
    assert!(matches!(failed.error, Error::Invalid(_)), "{failed:?}");
    assert!(!format!("{failed:?}").contains("canary"));
}
