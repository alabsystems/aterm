// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The one redirect hop (design §11.4): a GET or HEAD rebuilt fresh for an allowlisted CDN
//! origin, with only `Accept` and `User-Agent` and no credential; a write never hops; any
//! other location is refused; and the model's view of each request.

use std::time::Duration;

use agh::error::{Kind, Sub};
use agh::identity::Credential;
use agh::policy::Tier;
use agh::wire::door::AuthHeader;
use agh::wire::hop::{project_hop, rebuild};
use agh::wire::{Deadlines, Endpoints, Method, Origin, WireBody, WireRequest};

const CDN: &str = "https://objects.githubusercontent.com/github-production-release-asset/9?sig=ab";

/// A request from the api origin with an `Accept`, an api-version header and a header named
/// as the credential header is, as a caller might have set by hand.
fn original(method: Method) -> WireRequest<'static> {
    WireRequest::new(
        method,
        "https://api.github.com/repos/alabsystems/aterm/releases/assets/9",
        vec![
            ("accept".to_string(), "application/octet-stream".to_string()),
            ("X-GitHub-Api-Version".to_string(), "2022-11-28".to_string()),
            ("Authorization".to_string(), "x".to_string()),
            ("Cookie".to_string(), "c=1".to_string()),
        ],
        4096,
        Deadlines::for_transfer(1 << 30).with_backstop(Duration::from_secs(77)),
    )
}

#[test]
fn a_read_rebuilds_with_accept_and_user_agent_only() {
    let endpoints = Endpoints::github_com();
    for method in [Method::Get, Method::Head] {
        let req = original(method);
        let hopped = rebuild(&req, CDN, &endpoints, "agh/test").expect("a read hops");
        assert_eq!(hopped.method, method);
        assert_eq!(hopped.url, CDN);
        assert_eq!(
            hopped.headers,
            [
                ("Accept".to_string(), "application/octet-stream".to_string()),
                ("User-Agent".to_string(), "agh/test".to_string()),
            ]
        );
        assert!(hopped.auth.is_none());
        assert!(matches!(hopped.body, WireBody::Empty));
        assert_eq!(hopped.limit, req.limit);
        assert_eq!(hopped.deadlines, req.deadlines);
    }
    let bare = WireRequest::new(
        Method::Get,
        "https://api.github.com/x",
        Vec::new(),
        1,
        Deadlines::JSON,
    );
    let hopped = rebuild(&bare, CDN, &endpoints, "agh/test").expect("a read hops");
    assert_eq!(
        hopped.headers,
        [("User-Agent".to_string(), "agh/test".to_string())]
    );
}

#[test]
fn every_allowlisted_host_and_the_loopback_cdn_hop() {
    let endpoints = Endpoints::github_com();
    for host in endpoints.asset_hosts() {
        let location = format!("{}/a#frag", host.base());
        let hopped = rebuild(&original(Method::Get), &location, &endpoints, "agh/test")
            .expect("an asset host");
        assert_eq!(hopped.url, format!("{}/a", host.base()));
    }
    let loopback = Endpoints::loopback(1, 2, 3, 4);
    let hopped = rebuild(
        &original(Method::Get),
        "http://127.0.0.1:4/assets/9?sig=1",
        &loopback,
        "agh/test",
    )
    .expect("the loopback CDN");
    assert_eq!(hopped.url, "http://127.0.0.1:4/assets/9?sig=1");
}

#[test]
fn a_write_never_hops() {
    let endpoints = Endpoints::github_com();
    for method in [Method::Patch, Method::Post] {
        let error = rebuild(&original(method), CDN, &endpoints, "agh/test")
            .err()
            .expect("a write is refused");
        assert_eq!(error.kind, Kind::Conflict);
        assert_eq!(
            error.sub,
            Sub::Moved {
                to: CDN.to_string()
            }
        );
        assert!(error.why.contains(method.as_str()), "{}", error.why);
    }
}

#[test]
fn a_location_off_the_asset_hosts_is_refused() {
    let endpoints = Endpoints::github_com();
    for location in [
        "https://evil.example/asset",
        "http://objects.githubusercontent.com/asset",
        "https://objects.githubusercontent.com:8443/asset",
        "https://api.github.com/repos/o/r",
        "not a url",
        "ftp://objects.githubusercontent.com/asset",
    ] {
        let error = rebuild(&original(Method::Get), location, &endpoints, "agh/test")
            .err()
            .expect("refused");
        assert_eq!(error.kind, Kind::CouldNotRun, "{location}");
        assert_eq!(error.sub, Sub::Protocol, "{location}");
    }
    let shown = rebuild(
        &original(Method::Get),
        "https://evil.example:444/x",
        &endpoints,
        "agh/test",
    )
    .err()
    .expect("refused");
    let origin = Origin::parse("https://evil.example:444/x")
        .expect("parses")
        .0;
    assert!(shown.why.contains(&origin.base()), "{}", shown.why);
}

/// An unparsable Location is never echoed: it can carry userinfo and a signed query.
#[test]
fn an_unparsable_location_is_not_echoed() {
    let endpoints = Endpoints::github_com();
    // Joined at run time, so no literal in this file has an address's shape.
    let location = [
        "https://u:secret",
        "objects.githubusercontent.com/x?X-Amz-Signature=s",
    ]
    .join("@");
    let error = rebuild(&original(Method::Get), &location, &endpoints, "agh/test")
        .err()
        .expect("refused");
    assert_eq!(error.kind, Kind::CouldNotRun);
    assert_eq!(error.sub, Sub::Protocol);
    assert!(!error.why.contains("secret"), "{}", error.why);
    assert!(!error.why.contains("X-Amz-Signature"), "{}", error.why);
}

#[test]
fn the_model_sees_the_credential_on_a_request() {
    let endpoints = Endpoints::github_com();
    let plain = WireRequest::new(
        Method::Get,
        "https://api.github.com/x",
        vec![("Accept".to_string(), "*/*".to_string())],
        1,
        Deadlines::JSON,
    );
    assert_eq!(project_hop(&plain), (1, 0));
    let cred = Credential::for_tests("dev", Tier::Dev, agh_fake::token("hop").as_bytes());
    let header =
        AuthHeader::for_origin_for_tests(&cred, &endpoints, &Origin::https("api.github.com"))
            .expect("a header for the api origin");
    let mut carried = WireRequest::new(
        Method::Get,
        "https://api.github.com/x",
        Vec::new(),
        1,
        Deadlines::JSON,
    );
    carried.auth = Some(&header);
    assert_eq!(project_hop(&carried), (1, 1));
    let named = WireRequest::new(
        Method::Get,
        "https://api.github.com/x",
        vec![("authorization".to_string(), "x".to_string())],
        1,
        Deadlines::JSON,
    );
    assert_eq!(project_hop(&named), (1, 1));
    let hopped = rebuild(&carried, CDN, &endpoints, "agh/test").expect("hops");
    assert_eq!(project_hop(&hopped), (1, 0));
    assert_eq!(
        project_hop(&rebuild(&named, CDN, &endpoints, "agh/test").expect("hops")),
        (1, 0)
    );
}
