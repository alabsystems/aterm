// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use std::io::{Error as IoError, ErrorKind as K};

use super::{Phase, WireKind, kind_of};

fn io(kind: K, text: &str) -> aterm_http::Error {
    aterm_http::Error::Io(IoError::new(kind, text))
}

#[test]
fn every_row_of_the_transport_table() {
    let rows = [
        (
            Phase::NotSent,
            aterm_http::Error::Invalid("x".into()),
            WireKind::Local,
        ),
        (
            Phase::Answered,
            aterm_http::Error::Protocol("x".into()),
            WireKind::Protocol,
        ),
        (
            Phase::Sent,
            aterm_http::Error::TooLarge { limit: 1 },
            WireKind::TooLarge,
        ),
        (Phase::NotSent, io(K::Other, "proxy x"), WireKind::Proxy),
        (
            Phase::NotSent,
            aterm_http::Error::Protocol("proxy refused CONNECT with status 407".into()),
            WireKind::Proxy,
        ),
        (
            Phase::NotSent,
            aterm_http::Error::Protocol("proxy tunnel lost its socket".into()),
            WireKind::Proxy,
        ),
        (
            Phase::NotSent,
            aterm_http::Error::Invalid(
                "HTTPS_PROXY is set but is not a usable http:// proxy URL".into(),
            ),
            WireKind::Proxy,
        ),
        (
            Phase::NotSent,
            aterm_http::Error::Invalid(
                "https_proxy names an https:// proxy, which this client does not support; use an http:// proxy URL".into(),
            ),
            WireKind::Proxy,
        ),
        (
            Phase::NotSent,
            aterm_http::Error::Invalid("invalid header name: \"a b\"".into()),
            WireKind::Local,
        ),
        (
            Phase::NotSent,
            aterm_http::Error::Protocol("empty status line".into()),
            WireKind::Protocol,
        ),
        (
            Phase::Answered,
            aterm_http::Error::Protocol("proxy x".into()),
            WireKind::Protocol,
        ),
        (
            Phase::NotSent,
            io(K::TimedOut, "dns: no answer within 100ms"),
            WireKind::Timeout,
        ),
        (
            Phase::NotSent,
            io(K::NotFound, "dns: example.invalid: x"),
            WireKind::Dns,
        ),
        (
            Phase::NotSent,
            io(
                K::InvalidData,
                "TLS handshake failed: invalid peer certificate: x",
            ),
            WireKind::Trust,
        ),
        (Phase::NotSent, io(K::InvalidData, "x"), WireKind::Tls),
        (
            Phase::NotSent,
            io(K::TimedOut, "no bytes for 200ms"),
            WireKind::Timeout,
        ),
        (
            Phase::NotSent,
            io(K::PermissionDenied, "x"),
            WireKind::Revoked,
        ),
        (
            Phase::NotSent,
            io(K::ConnectionRefused, "x"),
            WireKind::Connect,
        ),
        (Phase::NotSent, io(K::Other, "x"), WireKind::Connect),
        (Phase::Sent, io(K::UnexpectedEof, "x"), WireKind::Truncated),
        (
            Phase::Sent,
            io(K::TimedOut, "no bytes for 200ms"),
            WireKind::Idle,
        ),
        (Phase::Answered, io(K::InvalidData, "x"), WireKind::Protocol),
        (Phase::Sent, io(K::ConnectionReset, "x"), WireKind::Reset),
        (Phase::NotSent, io(K::ConnectionReset, "x"), WireKind::Reset),
        (
            Phase::NotSent,
            io(K::ConnectionAborted, "x"),
            WireKind::Reset,
        ),
        (Phase::NotSent, io(K::BrokenPipe, "x"), WireKind::Reset),
    ];
    for (phase, error, kind) in rows {
        assert_eq!(kind_of(phase, &error), kind, "{phase:?} {error:?}");
    }
}
