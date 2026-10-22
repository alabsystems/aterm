// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The wire's types (the plan's §4.6.5–§4.6.7): origins, endpoints, deadlines, requests,
//! answers, the scripted transport and the credential door's public twin.

use std::io::Cursor;
use std::time::Duration;

use agh::identity::Credential;
use agh::policy::Tier;
use agh::wire::door::{AuthHeader, is_authorization};
use agh::wire::scripted::{Scripted, ScriptedTransport};
use agh::wire::{
    Deadlines, Endpoints, Method, Origin, OriginRole, Phase, Transport, WireBody, WireError,
    WireKind, WireRequest, WireResponse, wire_kind_of,
};

use crate::common;

/// A loopback port for the cases that name one.
const PORT: u16 = 47_002;

fn origin(url: &str) -> Origin {
    Origin::parse(url).expect("an origin").0
}

#[test]
fn origin_parse_reads_scheme_host_port_and_path() {
    let (o, path) = Origin::parse("HTTPS://API.GitHub.com/repos/x?y=1#frag").expect("parses");
    assert_eq!(
        (o.scheme(), o.host(), o.port()),
        ("https", "api.github.com", 443)
    );
    assert_eq!(path, "/repos/x?y=1");
    let (o, path) = Origin::parse("http://127.0.0.1").expect("parses");
    assert_eq!((o.scheme(), o.port()), ("http", 80));
    assert_eq!(path, "/");
    let (o, path) = Origin::parse("http://127.0.0.1:8080?q=1").expect("parses");
    assert_eq!(o.port(), 8080);
    assert_eq!(path, "/?q=1");
    for bad in [
        "https://:@api.github.com/",
        "http://[::1]/",
        "http://127.0.0.1:99999/",
        "ftp://example.com/",
    ] {
        assert!(Origin::parse(bad).is_none(), "{bad}");
    }
}

#[test]
fn origin_base_and_accessors() {
    let github = Origin::https("API.GitHub.com");
    assert_eq!(github.host(), "api.github.com");
    assert_eq!(github.base(), "https://api.github.com");
    assert_eq!(
        origin("https://api.github.com:8443").base(),
        "https://api.github.com:8443"
    );
    assert_eq!(origin("http://127.0.0.1:80").base(), "http://127.0.0.1");
}

#[test]
fn github_com_endpoints() {
    let e = Endpoints::github_com();
    assert_eq!(e.api(), &Origin::https("api.github.com"));
    assert_eq!(e.uploads(), &Origin::https("uploads.github.com"));
    assert_eq!(e.web(), &Origin::https("github.com"));
    let hosts: Vec<&str> = e.asset_hosts().iter().map(Origin::host).collect();
    assert_eq!(
        hosts,
        [
            "objects.githubusercontent.com",
            "release-assets.githubusercontent.com",
            "github-releases.githubusercontent.com"
        ]
    );
    assert_eq!(e.origin(OriginRole::Api), e.api());
    assert_eq!(e.origin(OriginRole::Uploads), e.uploads());
    assert_eq!(e.origin(OriginRole::Web), e.web());
    assert_eq!(e.origin(OriginRole::AssetCdn), &e.asset_hosts()[0]);
    for (o, role) in [
        (e.api().clone(), OriginRole::Api),
        (e.uploads().clone(), OriginRole::Uploads),
        (e.web().clone(), OriginRole::Web),
    ] {
        assert_eq!(e.role_of(&o), Some(role));
    }
    for o in e.asset_hosts() {
        assert_eq!(e.role_of(o), Some(OriginRole::AssetCdn), "{}", o.host());
    }
    for other in [
        "https://example.com",
        "http://api.github.com",
        "https://api.github.com:8443",
    ] {
        assert_eq!(e.role_of(&origin(other)), None, "{other}");
    }
}

#[test]
fn loopback_endpoints() {
    let e = Endpoints::loopback(1001, 1002, 1003, 1004);
    assert_eq!(
        e.role_of(&origin("http://127.0.0.1:1001")),
        Some(OriginRole::Api)
    );
    assert_eq!(
        e.role_of(&origin("http://127.0.0.1:1002")),
        Some(OriginRole::Uploads)
    );
    assert_eq!(
        e.role_of(&origin("http://127.0.0.1:1003")),
        Some(OriginRole::Web)
    );
    assert_eq!(
        e.role_of(&origin("http://127.0.0.1:1004")),
        Some(OriginRole::AssetCdn)
    );
    assert_eq!(e.origin(OriginRole::Web).port(), 1003);
    assert_eq!(
        e.url(OriginRole::Api, "/user"),
        "http://127.0.0.1:1001/user"
    );
}

#[test]
fn a_url_stays_on_its_origin() {
    let e = Endpoints::github_com();
    for (path, url) in [
        ("/user", "https://api.github.com/user"),
        (".evil.example/x", "https://api.github.com/.evil.example/x"),
        (":8443/x", "https://api.github.com/:8443/x"),
    ] {
        let built = e.url(OriginRole::Api, path);
        assert_eq!(built, url, "{path}");
        let (o, _) = Origin::parse(&built).expect("a URL Origin::parse reads");
        assert_eq!(e.role_of(&o), Some(OriginRole::Api), "{path}");
    }
}

#[test]
fn wire_request_new() {
    let headers = vec![("Accept".to_string(), "application/json".to_string())];
    let req = WireRequest::new(
        Method::Head,
        "https://api.github.com/user",
        headers.clone(),
        77,
        Deadlines::JSON,
    );
    assert_eq!(req.method, Method::Head);
    assert_eq!(req.url, "https://api.github.com/user");
    assert_eq!(req.headers, headers);
    assert_eq!(req.limit, 77);
    assert_eq!(req.deadlines, Deadlines::JSON);
    assert!(req.auth.is_none());
    assert!(matches!(req.body, WireBody::Empty));
}

#[test]
fn deadlines() {
    assert_eq!(
        (
            Deadlines::JSON.connect,
            Deadlines::JSON.idle,
            Deadlines::JSON.backstop
        ),
        (
            Duration::from_secs(10),
            Duration::from_secs(60),
            Duration::from_secs(60)
        )
    );
    assert_eq!(
        Deadlines::for_transfer(0).backstop,
        Duration::from_secs(600)
    );
    assert_eq!(
        Deadlines::for_transfer(4_096_000).backstop,
        Duration::from_secs(1000)
    );
    assert_eq!(
        Deadlines::JSON
            .with_backstop(Duration::from_secs(5))
            .backstop,
        Duration::from_secs(5)
    );
}

#[test]
fn wire_kind_of_each_row() {
    use std::io::{Error, ErrorKind as K};
    let cases = [
        (Error::other("dns: no such host"), WireKind::Dns),
        (
            Error::new(K::TimedOut, "dns: resolver timed out"),
            WireKind::Dns,
        ),
        (Error::new(K::UnexpectedEof, "eof"), WireKind::Truncated),
        (Error::new(K::InvalidData, "bad"), WireKind::Protocol),
        (
            Error::new(K::TimedOut, "no bytes for 60000ms"),
            WireKind::Idle,
        ),
        (
            Error::new(K::WouldBlock, "no bytes for 60000ms"),
            WireKind::Idle,
        ),
        (
            Error::new(K::TimedOut, "request deadline exceeded"),
            WireKind::Timeout,
        ),
        (Error::new(K::ConnectionReset, "reset"), WireKind::Reset),
        (Error::new(K::ConnectionAborted, "aborted"), WireKind::Reset),
        (Error::new(K::BrokenPipe, "pipe"), WireKind::Reset),
        (Error::new(K::PermissionDenied, "denied"), WireKind::Revoked),
        (
            Error::new(K::ConnectionRefused, "refused"),
            WireKind::Connect,
        ),
        (
            Error::other(aterm_http::Error::TooLarge { limit: 1 }),
            WireKind::TooLarge,
        ),
        (Error::other("something else"), WireKind::Local),
    ];
    for (error, kind) in cases {
        assert_eq!(wire_kind_of(&error), kind, "{error}");
    }
}

fn response(body: &[u8]) -> WireResponse {
    WireResponse {
        status: 200,
        headers: vec![
            ("Link".into(), "<a>".into()),
            ("link".into(), "<b>".into()),
            ("ETag".into(), "W/\"x\"".into()),
        ],
        body: Box::new(Cursor::new(body.to_vec())),
    }
}

#[test]
fn read_body_within_at_and_over_its_limit() {
    assert_eq!(response(b"abc").read_body(4).expect("within"), b"abc");
    assert_eq!(response(b"abcd").read_body(4).expect("at"), b"abcd");
    let over = response(b"abcde").read_body(4).expect_err("over");
    assert_eq!(
        (over.phase, over.kind),
        (Phase::Answered, WireKind::TooLarge)
    );
}

#[test]
fn header_and_header_all() {
    let r = response(b"");
    assert_eq!(r.header("LINK"), Some("<a>"));
    assert_eq!(r.header_all("link").collect::<Vec<_>>(), ["<a>", "<b>"]);
    assert_eq!(r.header("etag"), Some("W/\"x\""));
    assert_eq!(r.header("location"), None);
}

fn get<'a>(url: &str) -> WireRequest<'a> {
    WireRequest::new(Method::Get, url, Vec::new(), 1 << 20, Deadlines::JSON)
}

#[test]
fn scripted_transport_answers_in_order_and_records() {
    let transport = ScriptedTransport::new();
    transport.answer(200, &[("X-One", "1")], b"first");
    transport.push(Scripted::Fail(WireError {
        phase: Phase::Sent,
        kind: WireKind::Reset,
        detail: "reset".into(),
    }));
    let mut first = transport
        .round_trip(get("http://127.0.0.1/a"))
        .expect("the first answer");
    assert_eq!(first.status, 200);
    assert_eq!(first.read_body(100).expect("body"), b"first");
    let second = transport
        .round_trip(get("http://127.0.0.1/b"))
        .expect_err("the scripted failure");
    assert_eq!(second.kind, WireKind::Reset);
    let empty = transport
        .round_trip(get("http://127.0.0.1/c"))
        .expect_err("the empty queue");
    assert_eq!((empty.phase, empty.kind), (Phase::NotSent, WireKind::Local));
    assert_eq!(transport.remaining(), 0);
    let urls: Vec<String> = transport.requests().into_iter().map(|r| r.url).collect();
    assert_eq!(
        urls,
        [
            "http://127.0.0.1/a",
            "http://127.0.0.1/b",
            "http://127.0.0.1/c"
        ]
    );
}

#[test]
fn scripted_transport_reduces_the_credential_and_reads_a_body() {
    let endpoints = common::scripted_endpoints(PORT);
    let token = agh_fake::token("scripted");
    let cred = Credential::for_tests("dev", Tier::Dev, token.as_bytes());
    let header =
        AuthHeader::for_origin_for_tests(&cred, &endpoints, endpoints.api()).expect("a header");
    let transport = ScriptedTransport::new();
    transport.answer(204, &[], b"");
    let mut bytes = Cursor::new(b"streamed body".to_vec());
    let mut req = get(&endpoints.url(OriginRole::Api, "/x"));
    req.auth = Some(&header);
    req.body = WireBody::Reader {
        len: 13,
        reader: &mut bytes,
    };
    transport.round_trip(req).expect("answered");
    let seen = transport.requests();
    assert_eq!(seen[0].auth_profile.as_deref(), Some("dev"));
    assert_eq!(
        seen[0].auth_fingerprint.as_deref(),
        Some(cred.fingerprint())
    );
    assert_eq!(seen[0].body, b"streamed body");
    assert!(!format!("{seen:?}").contains(&token));
}

#[test]
fn the_door_over_github_com() {
    let endpoints = Endpoints::github_com();
    let token = agh_fake::token("door");
    let cred = Credential::for_tests("dev", Tier::Dev, token.as_bytes());
    for yes in ["https://api.github.com", "https://uploads.github.com"] {
        let o = origin(yes);
        let header = AuthHeader::for_origin_for_tests(&cred, &endpoints, &o).expect(yes);
        assert_eq!(header.origin(), &o);
        assert!(!format!("{header:?}").contains(&token));
    }
    let loopback = format!("http://127.0.0.1:{PORT}");
    for no in [
        "https://github.com",
        "https://objects.githubusercontent.com",
        "https://api.github.com:8443",
        "http://api.github.com",
        loopback.as_str(),
    ] {
        assert!(
            AuthHeader::for_origin_for_tests(&cred, &endpoints, &origin(no)).is_none(),
            "{no}"
        );
    }
}

#[test]
fn the_door_over_loopback() {
    let endpoints = common::scripted_endpoints(PORT);
    let cred = Credential::for_tests("dev", Tier::Dev, agh_fake::token("lo").as_bytes());
    for o in [endpoints.api(), endpoints.uploads()] {
        assert!(AuthHeader::for_origin_for_tests(&cred, &endpoints, o).is_some());
    }
    for o in [
        endpoints.web().clone(),
        endpoints.asset_hosts()[0].clone(),
        Origin::https("api.github.com"),
    ] {
        assert!(AuthHeader::for_origin_for_tests(&cred, &endpoints, &o).is_none());
    }
}

#[test]
fn is_authorization_in_any_case() {
    assert!(is_authorization("Authorization"));
    assert!(is_authorization("AUTHORIZATION"));
    assert!(!is_authorization("Authorization2"));
}

#[test]
fn methods() {
    let all = [
        (Method::Get, "GET", true),
        (Method::Head, "HEAD", true),
        (Method::Post, "POST", false),
        (Method::Patch, "PATCH", false),
        (Method::Put, "PUT", false),
        (Method::Delete, "DELETE", false),
    ];
    for (method, text, read) in all {
        assert_eq!((method.as_str(), method.is_read()), (text, read));
    }
}

#[test]
fn wire_kind_slugs() {
    let all = [
        (WireKind::Dns, "dns"),
        (WireKind::Connect, "connect"),
        (WireKind::Proxy, "proxy"),
        (WireKind::Tls, "tls"),
        (WireKind::Trust, "trust"),
        (WireKind::Timeout, "timeout"),
        (WireKind::Idle, "idle"),
        (WireKind::Reset, "reset"),
        (WireKind::Truncated, "truncated"),
        (WireKind::TooLarge, "too_large"),
        (WireKind::Protocol, "protocol"),
        (WireKind::Revoked, "revoked"),
        (WireKind::Local, "local"),
    ];
    for (kind, slug) in all {
        assert_eq!(kind.slug(), slug);
    }
}
