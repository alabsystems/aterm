// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Loopback tests for streamed request/response bodies, phases, and deadlines.

use std::ffi::OsString;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use aterm_http::proxy::EnvSource;
use aterm_http::{
    Body, Client, Connect, Deadline, Error, Method, Phase, ProxyMode, SendError, Trust,
};
use aterm_uds::secret::Secret;

const HANG_DETECTOR: Duration = Duration::from_secs(60);

const MIB: usize = 1024 * 1024;

struct NoEnv;

impl EnvSource for NoEnv {
    fn var_os(&self, _name: &str) -> Option<OsString> {
        None
    }
}

fn client() -> Client {
    Client::new(Trust::Roots(Vec::new()), ProxyMode::Direct, HANG_DETECTOR)
}

fn pattern(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| u8::try_from(i % 251).unwrap_or_default())
        .collect()
}

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

fn one_connection<T: Send + 'static>(
    handle: impl FnOnce(TcpStream) -> T + Send + 'static,
) -> (String, mpsc::Receiver<T>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((stream, _)) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        ) else {
            return;
        };
        let _ = stream.set_read_timeout(Some(HANG_DETECTOR));
        let _ = tx.send(handle(stream));
    });
    (format!("http://127.0.0.1:{port}/s"), rx)
}

fn answering(answer: Vec<u8>) -> String {
    one_connection(move |mut stream| {
        let _ = read_head(&mut stream);
        let _ = stream.write_all(&answer);
    })
    .0
}

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

struct InterruptedFragments<'a> {
    remaining: &'a [u8],
    interrupt: bool,
}

impl Read for InterruptedFragments<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if std::mem::take(&mut self.interrupt) {
            return Err(io::Error::from(ErrorKind::Interrupted));
        }
        let take = output.len().min(3);
        self.remaining.read(&mut output[..take])
    }
}

#[test]
fn collected_streams_preserve_responses_and_read_failures() {
    enum Expected {
        Body(u16, &'static [u8]),
        TooLarge,
        Short,
    }

    let cases: &[(&[u8], usize, Expected)] = &[
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nX-Checked: yes\r\n\r\n2\r\nok\r\n0\r\n\r\n",
            8,
            Expected::Body(200, b"ok"),
        ),
        (
            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 2\r\nX-Checked: yes\r\n\r\nno",
            8,
            Expected::Body(403, b"no"),
        ),
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
            1,
            Expected::TooLarge,
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n",
            1,
            Expected::TooLarge,
        ),
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nok",
            8,
            Expected::Short,
        ),
    ];
    let data = b"selected record bytes spanning reader fragments";
    for (answer, limit, expected) in cases {
        for streamed in [false, true] {
            let answer = answer.to_vec();
            let (endpoint, received) = one_connection(move |mut stream| {
                let head = read_head(&mut stream);
                let mut body = vec![0; declared(&head)];
                stream.read_exact(&mut body).unwrap();
                let _ = stream.write_all(&answer);
                body
            });
            let http = client();
            let request = http.post(&endpoint).limit(*limit);
            let result = if streamed {
                let mut reader = InterruptedFragments {
                    remaining: data,
                    interrupt: true,
                };
                request.send_body(Body::Reader {
                    len: data.len() as u64,
                    reader: &mut reader,
                })
            } else {
                request.send(data)
            };
            assert_eq!(received.recv_timeout(HANG_DETECTOR).unwrap(), data);
            match expected {
                Expected::Body(status, body) => {
                    let response = result.unwrap();
                    assert_eq!(response.status(), *status);
                    assert_eq!(response.header("x-checked"), Some("yes"));
                    assert_eq!(response.body(), *body);
                }
                Expected::TooLarge => assert!(
                    matches!(result, Err(Error::TooLarge { limit: 1 })),
                    "streamed={streamed}: {result:?}"
                ),
                Expected::Short => assert!(
                    matches!(result, Err(Error::Io(ref error)) if error.kind() == ErrorKind::UnexpectedEof),
                    "streamed={streamed}: {result:?}"
                ),
            }
        }
    }
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
    let text = error.to_string();
    assert!(text.contains(" 4 ") && text.contains(" 10 "), "{text}");
}

struct Failing(ErrorKind);

impl Read for Failing {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(self.0, "the local file went away"))
    }
}

#[test]
fn a_failing_body_reader_is_never_read_as_the_socket_failing() {
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
fn a_successful_consumer_finishes_the_http_body_before_returning() {
    let endpoint = answering(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nX-Seen: yes\r\n\r\n2\r\nok\r\n4\r\ntail\r\n0\r\nX-Trailer: done\r\n\r\n".to_vec(),
    );
    let exchange = client()
        .get(&endpoint)
        .exchange_with_env(Body::Empty, &NoEnv)
        .unwrap();
    assert_eq!(exchange.status(), 200);
    assert_eq!(exchange.header("x-seen"), Some("yes"));
    let prefix = exchange
        .consume_body(|body| {
            let mut prefix = [0; 2];
            body.read_exact(&mut prefix)?;
            Ok(prefix)
        })
        .expect("the tail and trailers complete the response");
    assert_eq!(&prefix, b"ok");
}

#[test]
fn a_successful_consumer_cannot_hide_a_late_body_failure() {
    enum Failure {
        Short,
        Protocol,
        TooLarge,
    }
    let cases: &[(&[u8], usize, Failure)] = &[
        (b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nok", 8, Failure::Short),
        (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\nzz\r\n", 8, Failure::Protocol),
        (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nokXX\r\n", 8, Failure::Protocol),
        (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\nX-Trailer: incomplete", 8, Failure::Short),
        (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n4\r\ntail\r\n0\r\n\r\n", 4, Failure::TooLarge),
        (b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\noktail", 4, Failure::TooLarge),
        (b"HTTP/1.1 200 OK\r\n\r\noktail", 4, Failure::TooLarge),
    ];
    for (answer, limit, expected) in cases {
        let endpoint = answering(answer.to_vec());
        let exchange = client()
            .get(&endpoint)
            .limit(*limit)
            .exchange_with_env(Body::Empty, &NoEnv)
            .unwrap();
        let mut consumed = false;
        let failed = exchange
            .consume_body(|_| {
                consumed = true;
                Ok("staged result")
            })
            .expect_err("the staged result cannot bypass HTTP completion");
        assert!(consumed);
        assert_eq!(failed.phase, Phase::Answered, "{failed:?}");
        match expected {
            Failure::Short => assert_eq!(io_error(&failed).kind(), ErrorKind::UnexpectedEof),
            Failure::Protocol => assert!(matches!(failed.error, Error::Protocol(_)), "{failed:?}"),
            Failure::TooLarge => assert!(
                matches!(failed.error, Error::TooLarge { limit: n } if n == *limit),
                "{failed:?}"
            ),
        }
    }
}

#[test]
fn an_empty_consumer_read_does_not_finish_a_close_delimited_body() {
    let endpoint = answering(b"HTTP/1.1 200 OK\r\n\r\nover the limit".to_vec());
    let failed = client()
        .get(&endpoint)
        .limit(2)
        .exchange_with_env(Body::Empty, &NoEnv)
        .unwrap()
        .consume_body(|body| {
            assert_eq!(body.read(&mut [])?, 0);
            Ok(())
        })
        .expect_err("a zero-length read did not observe the real EOF");
    assert_eq!(failed.phase, Phase::Answered);
    assert!(matches!(failed.error, Error::TooLarge { limit: 2 }));
}

#[derive(Debug)]
struct CompletionGuard(AtomicBool);

impl aterm_http::Guard for CompletionGuard {
    fn is_authorized(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[test]
fn a_consumer_that_observed_completion_needs_no_further_socket_read() {
    for answer in [
        &b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"[..],
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n",
        b"HTTP/1.1 200 OK\r\n\r\nok",
    ] {
        let guard = Arc::new(CompletionGuard(AtomicBool::new(true)));
        let endpoint = answering(answer.to_vec());
        let http = client();
        let exchange = http
            .get(&endpoint)
            .guard(Arc::clone(&guard) as Arc<dyn aterm_http::Guard>)
            .exchange_with_env(Body::Empty, &NoEnv)
            .unwrap();
        let bytes = exchange
            .consume_body(|body| {
                let mut bytes = Vec::new();
                body.read_to_end(&mut bytes)?;
                guard.0.store(false, Ordering::Release);
                Ok(bytes)
            })
            .expect("completed framing needs no new authorized read");
        assert_eq!(bytes, b"ok");
    }
}

#[test]
fn a_body_consumer_failure_preserves_its_type_and_answered_phase() {
    for timeout in [false, true] {
        let endpoint = answering(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec());
        let failed = client()
            .get(&endpoint)
            .exchange_with_env(Body::Empty, &NoEnv)
            .unwrap()
            .consume_body::<()>(|_| {
                Err(if timeout {
                    io::Error::new(ErrorKind::TimedOut, "consumer timeout")
                } else {
                    io::Error::other(Error::TooLarge { limit: 3 })
                })
            })
            .unwrap_err();
        assert_eq!(failed.phase, Phase::Answered);
        if timeout {
            assert_eq!(io_error(&failed).kind(), ErrorKind::TimedOut);
            assert_eq!(io_error(&failed).to_string(), "consumer timeout");
        } else {
            assert!(matches!(failed.error, Error::TooLarge { limit: 3 }));
        }
    }
}

#[test]
fn a_timeout_draining_an_answered_body_keeps_its_phase() {
    let (finish, hold) = mpsc::channel();
    let (endpoint, stopped) = one_connection(move |mut stream| {
        let _ = read_head(&mut stream);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n")
            .unwrap();
        // The missing body deliberately exceeds the idle policy; this wait only
        // keeps the socket alive until the client observes that policy failure.
        hold.recv_timeout(HANG_DETECTOR).unwrap();
    });
    let result = client()
        .get(&endpoint)
        .idle(Duration::from_millis(200))
        .exchange_with_env(Body::Empty, &NoEnv)
        .expect("the server supplies the complete response head")
        .consume_body(|_| Ok(()));
    finish.send(()).unwrap();
    stopped.recv_timeout(HANG_DETECTOR).unwrap();
    let failed = result.expect_err("a successful consumer cannot hide the idle timeout");
    assert_eq!(failed.phase, Phase::Answered);
    assert_eq!(io_error(&failed).kind(), ErrorKind::TimedOut);
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

#[cfg(unix)]
#[test]
fn an_answer_sent_while_the_body_was_still_going_out_is_read() {
    let (endpoint, _rx) = one_connection(|mut stream| {
        let _ = read_head(&mut stream);
        let _ = stream.set_nodelay(true);
        let _ = stream
            .write_all(b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 9\r\n\r\ntoo large");
    });
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
    const CONNECT: Duration = Duration::from_millis(500);
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
    let failed = client()
        .with_connector(Arc::new(Straight(loopback_addr(&endpoint))))
        .request(Method::Get, &endpoint)
        .idle(HANG_DETECTOR)
        .backstop(Duration::from_millis(300))
        .exchange_with_env(Body::Empty, &NoEnv)
        .expect_err("the trickle outlives the backstop");
    let error = io_error(&failed);
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    let text = error.to_string();
    assert!(!text.starts_with("no bytes for"), "{text}");
    assert!(
        text.contains("request") && text.contains("deadline") && !text.contains("connect"),
        "{text}"
    );
}

#[derive(Debug)]
struct Straight(SocketAddr);

impl Connect for Straight {
    fn connect(&self, _: &str, _: u16, _: Deadline) -> io::Result<TcpStream> {
        TcpStream::connect(self.0)
    }
}

#[test]
fn a_tls_handshake_that_never_answers_hits_the_connect_budget_unsent() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let roots = vec![include_bytes!("testdata/loopback-cert.der").to_vec()];
    let failed = Client::new(Trust::Roots(roots), ProxyMode::Direct, HANG_DETECTOR)
        .with_connector(Arc::new(Straight(listener.local_addr().unwrap())))
        .request(Method::Get, &format!("https://127.0.0.1:{port}/s"))
        .connect_within(Duration::from_millis(200))
        .exchange_with_env(Body::Empty, &NoEnv)
        .expect_err("the handshake cannot finish");
    assert_eq!(failed.phase, Phase::NotSent, "{failed:?}");
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
    for name in ["Proxy-Authorization", "Accept"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
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
            aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Ready,
                aterm_uds::AcceptedMode::Blocking,
            )
            .map(|_| ())
            .map_err(|error| error.kind()),
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
