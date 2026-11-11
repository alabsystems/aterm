// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const HANG_DETECTOR: Duration = Duration::from_secs(60);

#[test]
fn close_delimited_completion_requires_a_nonempty_eof_read_and_is_stable() {
    struct UntilEof {
        bytes: io::Cursor<&'static [u8]>,
        ended: bool,
    }
    impl Read for UntilEof {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            assert!(!self.ended, "the completed source was read again");
            let read = self.bytes.read(bytes)?;
            if read == 0 && !bytes.is_empty() {
                self.ended = true;
            }
            Ok(read)
        }
    }
    impl BufRead for UntilEof {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            self.bytes.fill_buf()
        }
        fn consume(&mut self, amount: usize) {
            self.bytes.consume(amount);
        }
    }
    let source = UntilEof {
        bytes: io::Cursor::new(b"body"),
        ended: false,
    };
    let mut body = Framed::new(source, Framing::ToEof, 4);
    assert_eq!(body.read(&mut []).unwrap(), 0);
    assert!(!body.is_finished());
    assert!(!body.reader.ended);
    assert_eq!(read_to_end(&mut body).unwrap(), b"body");
    assert!(body.is_finished());
    assert!(body.reader.ended);
    assert_eq!(body.read(&mut [0]).unwrap(), 0);
}

fn uri(text: &str) -> Uri {
    Uri::parse(text).unwrap()
}

fn proxy(text: &str) -> ProxyTarget {
    proxy::parse_proxy("HTTPS_PROXY", text).unwrap()
}

fn read_plain_request(reader: &mut impl Read) -> Vec<u8> {
    const LIMIT: usize = 1024;
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        assert!(
            head.len() < LIMIT,
            "fixture request head exceeded its bound"
        );
        let mut byte = [0];
        reader.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
    }
    let head = std::str::from_utf8(&head).unwrap();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            let value = value.trim();
            (name.eq_ignore_ascii_case("content-length")
                && !value.is_empty()
                && value.bytes().all(|b| b.is_ascii_digit()))
            .then(|| value.parse::<usize>().unwrap())
        })
        .expect("fixture request carries Content-Length");
    assert!(length <= LIMIT, "fixture request body exceeded its bound");
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    body
}

fn plain_stub(response: &'static [u8]) -> (String, std::thread::JoinHandle<Vec<u8>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/request", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        stream.set_read_timeout(Some(HANG_DETECTOR)).unwrap();
        stream.set_write_timeout(Some(HANG_DETECTOR)).unwrap();
        let body = read_plain_request(&mut stream);
        stream.write_all(response).unwrap();
        body
    });
    (endpoint, server)
}

#[test]
fn repeated_https_requests_and_client_clones_build_one_tls_config() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("https://{}/request", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            drop(
                aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .unwrap()
                .0,
            );
        }
    });
    let client = Client::new(
        Trust::Roots(vec![include_bytes!("testdata/tls/root.der").to_vec()]),
        ProxyMode::Direct,
        Duration::from_secs(2),
    );
    let clone = client.clone();

    assert!(
        Arc::ptr_eq(&client.trust, &clone.trust),
        "Client::clone must not copy configured CA bytes"
    );

    for requester in [&client, &clone] {
        requester
            .post(&endpoint)
            .send(b"{}")
            .expect_err("the TLS peer deliberately closes during handshake");
    }
    server.join().unwrap();

    assert!(Arc::ptr_eq(&client.tls_config, &clone.tls_config));
    assert_eq!(client.tls_config.builds(), 1);
}

#[test]
fn shared_trust_constructor_retains_the_callers_root_allocation() {
    let trust = Arc::new(Trust::Roots(vec![vec![1, 2, 3, 4]]));
    let client = Client::with_shared_trust(
        Arc::clone(&trust),
        ProxyMode::Direct,
        Duration::from_secs(2),
    );
    let clone = client.clone();

    assert!(Arc::ptr_eq(&trust, &client.trust));
    assert!(Arc::ptr_eq(&client.trust, &clone.trust));
}

#[test]
fn cached_tls_configuration_errors_are_shared_by_clones() {
    let client = Client::new(
        Trust::Roots(Vec::new()),
        ProxyMode::Direct,
        Duration::from_secs(2),
    );
    let clone = client.clone();
    let first = client.tls_config.get(client.trust.as_ref()).unwrap_err();
    let second = clone.tls_config.get(clone.trust.as_ref()).unwrap_err();
    assert_eq!(first, second);
    assert_eq!(client.tls_config.builds(), 1);
}

#[test]
fn plain_http_never_constructs_the_deferred_tls_configuration() {
    let (endpoint, server) =
        plain_stub(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
    let client = Client::new(Trust::Roots(Vec::new()), ProxyMode::Direct, HANG_DETECTOR);
    let response = client.post(&endpoint).send(b"{}").unwrap();
    assert_eq!(server.join().unwrap(), b"{}");
    assert_eq!(response.body(), b"ok");
    assert_eq!(client.tls_config.builds(), 0);
}

#[test]
fn a_plain_request_is_origin_form_with_host_and_length() {
    let head = render_request(
        Method::Post,
        &uri("http://127.0.0.1:11434/api/chat"),
        None,
        &[],
        Some(7),
    );
    assert!(head.starts_with("POST /api/chat HTTP/1.1\r\n"), "{head}");
    assert!(head.contains("Host: 127.0.0.1:11434\r\n"), "{head}");
    assert!(head.contains("Content-Length: 7\r\n"), "{head}");
    assert!(head.contains("Connection: close\r\n"), "{head}");
    assert!(head.ends_with("\r\n\r\n"), "{head}");
}

#[test]
fn plain_http_through_a_proxy_uses_absolute_form() {
    let head = render_request(
        Method::Post,
        &uri("http://api.test/v1/x"),
        Some(&proxy("http://proxy.test:3128")),
        &[],
        Some(0),
    );
    assert!(
        head.starts_with("POST http://api.test/v1/x HTTP/1.1\r\n"),
        "{head}"
    );
    assert!(!head.contains("Proxy-Authorization"), "{head}");
}

#[test]
fn the_proxy_credential_rides_the_absolute_form_and_never_the_tunnel() {
    let via = proxy("http://alice:s3cret@proxy.test:3128");
    let caller = [(
        "proxy-authorization".to_owned(),
        "Basic Y2FsbGVy".to_owned(),
    )];
    let head = render_request(
        Method::Post,
        &uri("http://api.test/v1/x"),
        Some(&via),
        &caller,
        Some(0),
    );
    assert_eq!(
        head.matches("Proxy-Authorization: Basic YWxpY2U6czNjcmV0\r\n")
            .count(),
        1,
        "{head}"
    );
    assert_eq!(
        head.to_ascii_lowercase()
            .matches("proxy-authorization:")
            .count(),
        1,
        "{head}"
    );
    let head = render_request(
        Method::Post,
        &uri("https://api.test/v1/x"),
        Some(&via),
        &caller,
        Some(0),
    );
    assert!(
        !head.to_ascii_lowercase().contains("proxy-authorization"),
        "{head}"
    );
}

/// Every secret byte of a head (a proxy credential, a secret header) is written into the one
/// buffer the send path scrubs, and that buffer is allocated at its final size: a size short
/// by one byte would grow it and leave a copy of what it held in the freed allocation. std's
/// `Vec::with_capacity` of bytes allocates exactly what it is asked for, and a buffer that a
/// short line outgrows is at least doubled, so here `capacity == len` holds only for an exact
/// size. The CONNECT a tunnel writes is built the same way.
#[test]
fn a_head_carrying_a_secret_is_one_buffer_at_its_final_size() {
    let via = proxy("http://alice:s3cret@proxy.test:3128");
    let secrets = [("X-Api-Key".to_owned(), Secret::new(b"k3y"))];
    let head = render_head(
        Method::Post,
        &uri("http://api.test/v1/x"),
        Some(&via),
        &headers(&[("Content-Type", "application/json")]),
        &secrets,
        Some(2),
    );
    assert_eq!(head.capacity(), head.len());
    let text = String::from_utf8(head).unwrap();
    assert_eq!(
        text.matches("\r\nProxy-Authorization: Basic YWxpY2U6czNjcmV0\r\n")
            .count(),
        1,
        "{text}"
    );
    assert!(text.ends_with("\r\nX-Api-Key: k3y\r\n\r\n"), "{text}");
    let tunnel = credentialed_head(
        "CONNECT api.test:443 HTTP/1.1\r\nHost: api.test:443\r\n",
        via.basic_auth(),
        &[],
    );
    assert_eq!(tunnel.capacity(), tunnel.len());
    assert_eq!(
        tunnel,
        b"CONNECT api.test:443 HTTP/1.1\r\nHost: api.test:443\r\n\
Proxy-Authorization: Basic YWxpY2U6czNjcmV0\r\n\r\n"
    );
}

#[test]
fn https_keeps_origin_form_even_through_a_proxy() {
    let head = render_request(
        Method::Post,
        &uri("https://api.test/v1/x"),
        Some(&proxy("http://proxy.test:3128")),
        &[],
        Some(0),
    );
    assert!(head.starts_with("POST /v1/x HTTP/1.1\r\n"), "{head}");
}

#[test]
fn a_caller_cannot_duplicate_a_generated_header() {
    let head = render_request(
        Method::Post,
        &uri("http://a.test/x"),
        None,
        &[
            ("Host".to_owned(), "evil.test".to_owned()),
            ("Content-Length".to_owned(), "0".to_owned()),
            ("tRaNsFeR-EnCoDiNg".to_owned(), "chunked".to_owned()),
            ("connection".to_owned(), "keep-alive".to_owned()),
            ("Content-Type".to_owned(), "application/json".to_owned()),
        ],
        Some(9),
    );
    assert_eq!(head.matches("Host:").count(), 1, "{head}");
    assert!(head.contains("Host: a.test\r\n"), "{head}");
    assert_eq!(head.matches("Content-Length:").count(), 1, "{head}");
    assert!(head.contains("Content-Length: 9\r\n"), "{head}");
    assert!(
        !head.to_ascii_lowercase().contains("transfer-encoding:"),
        "{head}"
    );
    assert_eq!(
        head.to_lowercase().matches("connection:").count(),
        1,
        "{head}"
    );
    assert!(head.contains("Connection: close\r\n"), "{head}");
    assert!(
        head.contains("Content-Type: application/json\r\n"),
        "{head}"
    );
}

#[test]
fn header_injection_is_rejected() {
    for (name, value) in [
        ("X-A", "ok\r\nX-Evil: 1"),
        ("X-A", "ok\nX-Evil: 1"),
        ("X-A", "ok\0"),
        ("X\r\nEvil", "1"),
        ("", "1"),
        ("X:Y", "1"),
    ] {
        assert!(
            validate_header(name, value).is_err(),
            "{name:?}: {value:?} must be rejected"
        );
    }
    assert!(validate_header("Authorization", "Bearer abc.def-123").is_ok());
    assert!(validate_header("Content-Type", "application/json").is_ok());
}

#[test]
fn header_names_use_the_same_token_grammar_in_both_directions() {
    const PUNCTUATION: &str = "!#$%&'*+-.^_`|~";
    for byte in 0u8..=127 {
        let name = format!("X{}Field", char::from(byte));
        let valid = byte.is_ascii_alphanumeric() || PUNCTUATION.contains(char::from(byte));
        assert_eq!(validate_header(&name, "ok").is_ok(), valid, "{name:?}");
        let valid_response = valid || byte == b':';
        let raw = format!("HTTP/1.1 204 No Content\r\n{name}: ok\r\n\r\n");
        assert_eq!(
            parse(raw.as_bytes(), 1024).is_ok(),
            valid_response,
            "{name:?}"
        );
    }
    for name in ["", "Content-Length ", " Content-Length", "X-é"] {
        assert!(validate_header(name, "0").is_err(), "{name:?}");
        let raw = format!("HTTP/1.1 204 No Content\r\n{name}: 0\r\n\r\n");
        assert!(
            matches!(parse(raw.as_bytes(), 1024), Err(Error::Protocol(_))),
            "{name:?}"
        );
    }
}

#[test]
fn response_headers_preserve_values_and_reject_controls() {
    for byte in (0u8..=31).chain([127]).filter(|b| *b != b'\t') {
        let mut raw = b"HTTP/1.1 204 No Content\r\nX-Field: before".to_vec();
        raw.push(byte);
        raw.extend_from_slice(b"after\r\n\r\n");
        assert!(
            matches!(parse(&raw, 1024), Err(Error::Protocol(_))),
            "{byte}"
        );
        assert!(validate_header("X-Field", &format!("before{}after", char::from(byte))).is_err());
    }
    let raw = "HTTP/1.1 204 No Content\r\nX-Field: \t\u{a0}value\u{a0}\t \r\n\r\n";
    let response = parse(raw.as_bytes(), 1024).unwrap();
    assert_eq!(response.header("x-field"), Some("\u{a0}value\u{a0}"));
}

fn parse(raw: &[u8], limit: usize) -> Result<Response, Error> {
    read_response_from(BufReader::new(raw), Method::Get, limit)
}

fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn framing_refuses_every_response_whose_end_is_ambiguous() {
    assert_eq!(
        decide_framing(Method::Get, 200, &headers(&[])).unwrap(),
        Framing::ToEof
    );
    assert_eq!(
        decide_framing(Method::Get, 200, &headers(&[("Content-Length", "7")])).unwrap(),
        Framing::Length(7)
    );
    assert_eq!(
        decide_framing(
            Method::Get,
            200,
            &headers(&[("Transfer-Encoding", "chunked")])
        )
        .unwrap(),
        Framing::Chunked
    );
    assert_eq!(
        decide_framing(
            Method::Get,
            200,
            &headers(&[("Content-Length", "5"), ("content-length", "5")])
        )
        .unwrap(),
        Framing::Length(5)
    );
    assert_eq!(
        decide_framing(Method::Get, 204, &headers(&[("Content-Length", "5")])).unwrap(),
        Framing::Empty
    );
    for ambiguous in [
        vec![("Content-Length", "5"), ("Content-Length", "9")],
        vec![("Content-Length", "5"), ("Transfer-Encoding", "chunked")],
        vec![("Content-Length", "5"), ("Transfer-Encoding", "gzip")],
        vec![
            ("Transfer-Encoding", "gzip"),
            ("Transfer-Encoding", "chunked"),
        ],
        vec![
            ("Transfer-Encoding", "gzip"),
            ("Transfer-Encoding", "chunked"),
            ("Content-Length", "5"),
        ],
        vec![("Content-Length", "+5")],
        // Direct unit precondition on `decide_framing`: callers must pass OWS-trimmed
        // field values; on the wire `read_header_fields` trims SP/HTAB first (checked below).
        vec![("Content-Length", "5 ")],
        vec![("Content-Length", "")],
    ] {
        assert!(
            matches!(
                decide_framing(Method::Get, 200, &headers(&ambiguous)),
                Err(Error::Protocol(_))
            ),
            "{ambiguous:?} must be refused"
        );
    }
    assert_eq!(
        parse(b"HTTP/1.1 200 OK\r\nContent-Length: 5 \r\n\r\nhello", 1024)
            .unwrap()
            .body(),
        b"hello",
        "wire parser trims trailing OWS before calling decide_framing"
    );
    let message = |value: &str| match decide_framing(
        Method::Get,
        200,
        &headers(&[("Content-Length", value)]),
    ) {
        Err(Error::Protocol(m)) => m,
        other => panic!("{value:?}: {other:?}"),
    };
    let bad = message("+5");
    assert!(bad.contains("\"+5\""), "{bad}");
    let huge = "9".repeat(40);
    let too_large = message(&huge);
    assert!(too_large.contains(&huge), "{too_large}");
    assert_ne!(
        bad.replace("\"+5\"", ""),
        too_large.replace(&format!("{huge:?}"), "")
    );
}

#[test]
fn the_parser_frames_through_decide_framing() {
    for raw in [
        &b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 9\r\n\r\nhi"[..],
        &b"HTTP/1.1 200 OK\r\nContent-Length: +2\r\n\r\nhi"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n0\r\n\r\n"[..],
    ] {
        assert!(
            matches!(parse(raw, 1024), Err(Error::Protocol(_))),
            "{:?} must be refused",
            String::from_utf8_lossy(raw)
        );
    }
}

#[test]
fn chunked_framing_reassembles_and_skips_extensions_and_trailers() {
    let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
5;name=v\r\nhello\r\n6\r\n world\r\n0\r\nX-Trailer: t\r\n\r\n";
    let response = parse(raw, 1024).unwrap();
    assert_eq!(response.body(), b"hello world");
}

#[test]
fn a_chunk_size_is_hex_digits_and_nothing_else() {
    for size in ["+2", " 2", "\t2", "0x2", ""] {
        let raw = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{size}\r\nhi\r\n0\r\n\r\n"
        );
        assert!(
            matches!(parse(raw.as_bytes(), 1024), Err(Error::Protocol(_))),
            "chunk size {size:?} must be refused"
        );
    }
    let spaced_extension =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2 ;name=v\r\nhi\r\n0\r\n\r\n";
    assert_eq!(parse(spaced_extension, 1024).unwrap().body(), b"hi");
    for bad_framing_raw in [
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\nhi\r\n0\r\n\r\n"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\n0\r\n\r\n"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2;ext=\x00\r\nhi\r\n0\r\n\r\n"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n0\r\nX-Trailer: \x7f\r\n\r\n"[..],
    ] {
        assert!(
            matches!(parse(bad_framing_raw, 1024), Err(Error::Protocol(_))),
            "bad chunk framing {bad_framing_raw:?} must be refused"
        );
    }
}

#[test]
fn eof_framing_when_no_length_is_given() {
    let raw = b"HTTP/1.1 200 OK\r\n\r\nbare body";
    assert_eq!(parse(raw, 1024).unwrap().body(), b"bare body");
}

#[test]
fn a_body_over_the_limit_errors_rather_than_truncating() {
    let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n";
    assert!(matches!(parse(raw, 10), Err(Error::TooLarge { limit: 10 })));

    let chunked =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n20\r\n00000000000000000000000000000000\r\n0\r\n\r\n";
    assert!(matches!(parse(chunked, 10), Err(Error::TooLarge { .. })));

    let eof = b"HTTP/1.1 200 OK\r\n\r\n0123456789abcdef";
    assert!(matches!(parse(eof, 10), Err(Error::TooLarge { .. })));
}

#[test]
fn non_2xx_status_is_reported_not_hidden() {
    let raw = b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n";
    let response = parse(raw, 1024).unwrap();
    assert_eq!(response.status(), 503);
    assert!(!response.is_success());
}

#[test]
fn malformed_status_lines_are_rejected() {
    for raw in [
        &b"garbage\r\n\r\n"[..],
        &b"HTTP/2 200 OK\r\n\r\n"[..],
        &b"HTTP/1. 200 OK\r\n\r\n"[..],
        &b"HTTP/1.10 200 OK\r\n\r\n"[..],
        &b"HTTP/1.x 200 OK\r\n\r\n"[..],
        &b"HTTP/1.1 notanumber OK\r\n\r\n"[..],
        &b"HTTP/1.1 200 OK\x00\r\n\r\n"[..],
        &b"HTTP/1.1 200 OK\x7f\r\n\r\n"[..],
    ] {
        assert!(parse(raw, 1024).is_err(), "{raw:?} must not parse");
    }
}

#[test]
fn interim_1xx_responses_are_skipped_to_the_final_one() {
    let raw = b"HTTP/1.1 100 Continue\r\n\r\n\
HTTP/1.1 103 Early Hints\r\nLink: </style.css>; rel=preload\r\n\r\n\
HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
    let response = parse(raw, 1024).unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"ok");
    assert_eq!(response.header("link"), None);
    let upgrade = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: h2c\r\n\r\n";
    assert!(matches!(parse(upgrade, 1024), Err(Error::Protocol(_))));
    let endless = b"HTTP/1.1 100 Continue\r\n\r\n".repeat(MAX_HEADER_BYTES);
    assert!(matches!(parse(&endless, 1024), Err(Error::Protocol(_))));
}

#[test]
fn a_peer_that_closes_unanswered_is_a_transport_failure() {
    assert!(
        matches!(parse(b"", 1024), Err(Error::Io(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof),
        "{:?}",
        parse(b"", 1024)
    );
    for raw in [
        &b"HTTP/1.1 200 OK\r\n"[..],
        b"HTTP/1.1 200 OK\r\n\r",
        b"HTTP/1.1 200 OK\r\nX-A: 1\r\n\r",
        b"HTTP/1.1 100 Continue\r\n\r\n",
        b"\r\n",
    ] {
        assert!(
            matches!(parse(raw, 1024), Err(Error::Protocol(_))),
            "{raw:?}"
        );
    }
    // Control: the same heads with their last '\n' are whole, and parse.
    for raw in [
        &b"HTTP/1.1 200 OK\r\n\r\n"[..],
        b"HTTP/1.1 200 OK\r\nX-A: 1\r\n\r\n",
    ] {
        assert_eq!(
            parse(raw, 1024).map(|r| r.status()).ok(),
            Some(200),
            "{raw:?}"
        );
    }
}

#[test]
fn bodiless_statuses_end_at_their_header_block_whatever_they_declare() {
    for raw in [
        &b"HTTP/1.1 204 No Content\r\nContent-Length: 5\r\n\r\n"[..],
        &b"HTTP/1.1 304 Not Modified\r\nContent-Length: 5\r\n\r\n"[..],
        &b"HTTP/1.1 304 Not Modified\r\nTransfer-Encoding: chunked\r\n\r\n"[..],
    ] {
        let response = parse(raw, 1024)
            .unwrap_or_else(|error| panic!("{:?}: {error:?}", String::from_utf8_lossy(raw)));
        assert!(response.body().is_empty());
    }
}

#[test]
fn a_transfer_coding_this_client_cannot_decode_is_refused() {
    for raw in [
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\nContent-Length: 2\r\n\r\nhi"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\nhi"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked, gzip\r\n\r\n2\r\nhi\r\n0\r\n\r\n"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n0\r\n\r\n"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked, chunked\r\n\r\n2\r\nhi\r\n0\r\n\r\n"[..],
    ] {
        assert!(
            matches!(parse(raw, 1024), Err(Error::Protocol(_))),
            "{:?} must be refused",
            String::from_utf8_lossy(raw)
        );
    }
    let listed = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: , chunked\r\n\r\n2\r\nhi\r\n0\r\n\r\n";
    assert_eq!(parse(listed, 1024).unwrap().body(), b"hi");
}

#[test]
fn a_status_code_is_exactly_three_digits() {
    for raw in [
        &b"HTTP/1.1 +200 OK\r\nContent-Length: 0\r\n\r\n"[..],
        &b"HTTP/1.1 0200 OK\r\nContent-Length: 0\r\n\r\n"[..],
        &b"HTTP/1.1 20 OK\r\nContent-Length: 0\r\n\r\n"[..],
        &b"HTTP/1.1 999 OK\r\nContent-Length: 0\r\n\r\n"[..],
    ] {
        assert!(
            matches!(parse(raw, 1024), Err(Error::Protocol(_))),
            "{:?} must be refused",
            String::from_utf8_lossy(raw)
        );
    }
}

#[test]
fn an_unterminated_header_block_does_not_hang() {
    let raw = b"HTTP/1.1 200 OK\r\nX-A: 1\r\n";
    assert!(parse(raw, 1024).is_err());
}

#[test]
fn a_declared_length_near_usize_max_is_a_short_body_not_an_allocation() {
    for declared in [usize::MAX, usize::MAX / 2] {
        let raw = format!("HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\n\r\nx");
        match parse(raw.as_bytes(), usize::MAX) {
            Err(Error::Io(error)) => {
                assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof, "{error}");
            }
            other => panic!("{declared}: {other:?}"),
        }
    }
}

struct UncleanClose<'a>(&'a [u8]);

impl Read for UncleanClose<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.0.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "peer closed connection without sending TLS close_notify",
            ));
        }
        let n = buf.len().min(self.0.len());
        buf[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        Ok(n)
    }
}

#[test]
fn an_unclean_tls_close_in_the_head_reads_as_the_plain_eof() {
    let over_tls = read_final_head(&mut BufReader::new(UncleanClose(b""))).unwrap_err();
    let over_tcp = read_final_head(&mut BufReader::new(&b""[..])).unwrap_err();
    let (Error::Io(over_tls), Error::Io(over_tcp)) = (&over_tls, &over_tcp) else {
        panic!("{over_tls:?} / {over_tcp:?}");
    };
    assert_eq!(over_tls.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(
        (over_tls.kind(), over_tls.to_string()),
        (over_tcp.kind(), over_tcp.to_string())
    );
    for (unclean, plain) in [
        (
            &b"HTTP/1.1 200 OK\r\nX-A:"[..],
            &b"HTTP/1.1 200 OK\r\nX-A:"[..],
        ),
        (&b"HTTP/1.1 200 OK\r\n"[..], &b"HTTP/1.1 200 OK\r\n"[..]),
        (&b"HTTP/1.1 200 OK\r\n\r"[..], &b"HTTP/1.1 200 OK\r\n\r"[..]),
    ] {
        let over_tls = read_final_head(&mut BufReader::new(UncleanClose(unclean)));
        let over_tcp = read_final_head(&mut BufReader::new(plain));
        assert!(
            matches!(
                (&over_tls, &over_tcp),
                (Err(Error::Protocol(a)), Err(Error::Protocol(b))) if a == b
            ),
            "{over_tls:?} / {over_tcp:?}"
        );
    }
}

struct Interrupting(std::collections::VecDeque<&'static [u8]>, bool);

impl Read for Interrupting {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if std::mem::take(&mut self.1) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let Some(part) = self.0.pop_front() else {
            return Ok(0);
        };
        buf[..part.len()].copy_from_slice(part);
        self.1 = true;
        Ok(part.len())
    }
}

#[test]
fn an_interrupted_read_inside_a_chunk_size_line_loses_no_framing() {
    let inner = Interrupting(
        [&b"1"[..], b"0\r\n0123456789abcdef\r\n0\r\n\r\n"].into(),
        false,
    );
    let mut body = Vec::new();
    Framed::new(BufReader::new(inner), Framing::Chunked, usize::MAX)
        .read_to_end(&mut body)
        .unwrap();
    assert_eq!(body, b"0123456789abcdef");
}

#[test]
fn an_interrupted_read_in_the_head_is_waited_out() {
    let inner = Interrupting(
        [&b"HTTP/1.1 2"[..], b"00 OK\r\nX-A: 1\r\n\r\n"].into(),
        true,
    );
    let head = read_final_head(&mut BufReader::new(inner)).unwrap();
    assert_eq!(head.code, 200);
    assert_eq!(head.headers, vec![("X-A".to_owned(), "1".to_owned())]);
}
