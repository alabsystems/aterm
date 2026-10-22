// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `HttpTransport::direct` on agh-fake: headers, the credential's origin, hops, moves.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use agh::identity::Credential;
use agh::policy::{RouteRequest, RouteTarget, Router, Tier};
use agh::retry::RetryPolicy;
use agh::wire::door::AuthHeader;
use agh::wire::http::HttpTransport;
use agh::wire::{
    Deadlines, Method, OriginRole, Phase, Transport, WireError, WireKind, WireRequest,
};
use agh::{Accept, Call, Kind, Sub};
use agh_fake::{AssetSpec, FakeGithub, FakeOrigin, Fault, Match, ReleaseSpec, RepoSpec};

use crate::common;

/// A body limit for the transport-level requests.
const LIMIT: u64 = 1 << 20;

fn read_of(text: &str) -> RouteRequest {
    RouteRequest::read(RouteTarget::Repo(common::slug(text)))
}

/// A fake holding the public `fixture-dev/repo`.
fn fake_with_repo() -> FakeGithub {
    let fake = FakeGithub::start();
    fake.add_repo("fixture-dev/repo", RepoSpec::public());
    fake
}

/// An anonymous client over `HttpTransport::direct` on `fake`.
fn anonymous(fake: &FakeGithub) -> agh::Github {
    common::github(Router::empty(), fake)
}

/// Every request `fake` saw carried no `Content-Length` (each test's requests are reads), and
/// the fake recorded no breach of its wire rules.
fn assert_clean(fake: &FakeGithub) {
    for seen in fake.seen() {
        assert_eq!(seen.header("content-length"), None, "{seen:?}");
    }
    fake.assert_clean();
}

/// Deadlines whose idle is 200 ms.
fn short_idle() -> Deadlines {
    Deadlines {
        idle: Duration::from_millis(200),
        ..Deadlines::JSON
    }
}

/// A published release with one asset on `repo`; the asset's id.
fn one_asset(fake: &FakeGithub, repo: &str) -> u64 {
    let release = fake.add_release(repo, ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"));
    fake.add_asset(release, AssetSpec::uploaded("a.bin", b"the asset bytes"))
}

/// The asset download as `call_stream` makes it: Accept octet-stream, one hop.
fn asset_call(repo: &str, id: u64) -> Call {
    Call::get(format!("/repos/{repo}/releases/assets/{id}"), read_of(repo))
        .accept(Accept::OctetStream)
        .hop(true)
}

#[test]
fn the_api_origin_gets_user_agent_api_version_and_accept() {
    let fake = fake_with_repo();
    let gh = anonymous(&fake);
    let answer = gh
        .call(&Call::get(
            "/repos/fixture-dev/repo",
            read_of("fixture-dev/repo"),
        ))
        .expect("the repository");
    assert_eq!(answer.status, 200);
    let json = answer.json().expect("JSON");
    assert_eq!(
        json.get("full_name").and_then(aterm_json::Value::as_str),
        Some("fixture-dev/repo")
    );
    assert!(
        answer
            .request_id
            .as_deref()
            .is_some_and(|id| id.starts_with("FAKE:"))
    );
    assert!(answer.etag.is_some());
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].origin, FakeOrigin::Api);
    assert_eq!(seen[0].method, "GET");
    assert_eq!(seen[0].path_and_query, "/repos/fixture-dev/repo");
    assert_eq!(
        seen[0].header("user-agent"),
        Some(format!("agh/{} (test)", agh::VERSION).as_str())
    );
    assert_eq!(seen[0].header("x-github-api-version"), Some("2022-11-28"));
    assert_eq!(
        seen[0].header("accept"),
        Some("application/vnd.github+json")
    );
    assert!(!seen[0].had_authorization);
    assert_clean(&fake);
}

#[test]
fn the_web_origin_gets_no_api_version() {
    let fake = fake_with_repo();
    let gh = anonymous(&fake);
    let call = Call::get("/fixture-dev/repo", read_of("fixture-dev/repo"))
        .origin(OriginRole::Web)
        .answer_any(true);
    gh.call(&call).expect("the web answers");
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].origin, FakeOrigin::Web);
    assert_eq!(seen[0].header("x-github-api-version"), None);
    assert!(seen[0].header("user-agent").is_some());
    assert_clean(&fake);
}

#[test]
fn an_anonymous_asset_download_hops_to_the_cdn() {
    let fake = fake_with_repo();
    let id = one_asset(&fake, "fixture-dev/repo");
    let gh = anonymous(&fake);
    let mut streamed = gh
        .call_stream(&asset_call("fixture-dev/repo", id))
        .expect("the asset");
    let mut bytes = Vec::new();
    streamed.body.read_to_end(&mut bytes).expect("the bytes");
    assert_eq!(bytes, b"the asset bytes");
    assert!(streamed.hopped);
    assert_eq!(streamed.requests, 2);
    assert!(gh.meter().hopped);
    let seen = fake.seen();
    let origins: Vec<FakeOrigin> = seen.iter().map(|s| s.origin).collect();
    assert_eq!(origins, [FakeOrigin::Api, FakeOrigin::Cdn]);
    assert_eq!(seen[1].header("accept"), Some("application/octet-stream"));
    assert_eq!(seen[1].header("x-github-api-version"), None);
    assert_clean(&fake);
}

#[test]
fn a_second_hop_is_refused() {
    let fake = fake_with_repo();
    let id = one_asset(&fake, "fixture-dev/repo");
    fake.fault(Match::cdn(), Fault::SecondHop);
    let gh = anonymous(&fake);
    let error = gh
        .call_stream(&asset_call("fixture-dev/repo", id))
        .expect_err("a second redirect");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::CouldNotRun, Sub::Protocol)
    );
    let origins: Vec<FakeOrigin> = fake.seen().iter().map(|s| s.origin).collect();
    assert_eq!(origins, [FakeOrigin::Api, FakeOrigin::Cdn]);
    assert_clean(&fake);
}

#[test]
fn a_small_limit_still_classifies_a_failed_answer_s_body() {
    let fake = fake_with_repo();
    let id = one_asset(&fake, "fixture-dev/repo");
    fake.fault(
        Match::api("/repos/fixture-dev/repo/releases/assets"),
        Fault::Status {
            status: 403,
            body: r#"{"message":"You have exceeded a secondary rate limit."}"#.into(),
            headers: vec![],
        },
    );
    let gh = anonymous(&fake);
    let error = gh
        .call_stream(
            &asset_call("fixture-dev/repo", id)
                .limit(4)
                .retry(Some(RetryPolicy::NONE)),
        )
        .expect_err("a secondary limit");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::RateLimited, Sub::Secondary)
    );
    assert!(
        error
            .github
            .as_ref()
            .and_then(|g| g.message.as_deref())
            .is_some_and(|m| m.contains("secondary rate limit"))
    );
    assert_clean(&fake);
}

#[test]
fn an_asset_past_the_call_limit_is_too_large_after_the_hop() {
    let fake = fake_with_repo();
    let id = one_asset(&fake, "fixture-dev/repo");
    let gh = anonymous(&fake);
    let mut streamed = gh
        .call_stream(&asset_call("fixture-dev/repo", id).limit(4))
        .expect("the hop answers");
    assert!(streamed.hopped);
    let mut bytes = Vec::new();
    let error = streamed
        .body
        .read_to_end(&mut bytes)
        .expect_err("past the limit");
    assert_eq!(agh::wire::wire_kind_of(&error), WireKind::TooLarge);
    assert_clean(&fake);
}

/// A whole 2xx past the call's limit, over the shipped transport: under the 64 KiB wire floor
/// (`limit(100)`) and over it (`limit(70_000)`), the refusal keeps the status, the fake's
/// request id and the rate, and its why names the call's own limit. aterm-http, held to the
/// call's limit itself, would refuse the declared length before the cut and drop the head.
#[test]
fn a_whole_read_past_the_call_limit_keeps_its_head() {
    let fake = fake_with_repo();
    fake.put_file("fixture-dev/repo", "main", "big.bin", &[b'a'; 70_001]);
    let gh = anonymous(&fake);
    for limit in [100_u64, 70_000] {
        let call = Call::get(
            "/repos/fixture-dev/repo/contents/big.bin",
            read_of("fixture-dev/repo"),
        )
        .accept(Accept::Raw)
        .limit(limit)
        .retry(Some(RetryPolicy::NONE));
        let error = gh.call(&call).expect_err("past the limit");
        assert_eq!(
            (error.kind, error.sub.clone()),
            (Kind::CouldNotRun, Sub::Wire(WireKind::TooLarge)),
            "{limit}"
        );
        assert!(
            error.why.contains(&format!("{limit} bytes")),
            "{limit}: {}",
            error.why
        );
        let http = error.http.as_ref().expect("the answer's head");
        assert_eq!(http.status, 200, "{limit}");
        assert!(
            http.request_id
                .as_deref()
                .is_some_and(|id| id.starts_with("FAKE:")),
            "{limit}: {http:?}"
        );
        assert!(error.rate.is_some(), "{limit}: no rate");
    }
    assert_eq!(fake.seen().len(), 2);
    assert_clean(&fake);
}

#[test]
fn a_hang_at_the_cdn_past_the_idle_is_idle() {
    let fake = fake_with_repo();
    let id = one_asset(&fake, "fixture-dev/repo");
    fake.fault(Match::cdn(), Fault::Hang(common::HANG));
    let gh = anonymous(&fake);
    let error = gh
        .call_stream(&asset_call("fixture-dev/repo", id).deadlines(short_idle()))
        .expect_err("idle");
    assert_eq!(error.sub, Sub::Wire(WireKind::Idle));
    assert_eq!(error.kind, Kind::CouldNotRun);
    assert_clean(&fake);
}

#[test]
fn a_closed_hop_is_retried_from_the_api_origin() {
    let fake = fake_with_repo();
    let id = one_asset(&fake, "fixture-dev/repo");
    fake.fault(Match::cdn(), Fault::Close);
    let gh = anonymous(&fake);
    let mut streamed = gh
        .call_stream(&asset_call("fixture-dev/repo", id).retry(Some(RetryPolicy::STANDARD)))
        .expect("the asset on the second attempt");
    let mut bytes = Vec::new();
    streamed.body.read_to_end(&mut bytes).expect("the bytes");
    assert_eq!(bytes, b"the asset bytes");
    assert!(streamed.hopped);
    assert_eq!(streamed.requests, 4);
    assert_eq!(gh.meter().requests, 4);
    let origins: Vec<FakeOrigin> = fake.seen().iter().map(|s| s.origin).collect();
    assert_eq!(
        origins,
        [
            FakeOrigin::Api,
            FakeOrigin::Cdn,
            FakeOrigin::Api,
            FakeOrigin::Cdn
        ]
    );
    assert_eq!(gh.fx().slept(), [Duration::from_secs(2)]);
    assert_clean(&fake);
}

#[test]
fn a_renamed_repository_is_followed_and_noted() {
    let fake = fake_with_repo();
    fake.rename_repo("fixture-dev/repo", "fixture-dev/renamed");
    let gh = anonymous(&fake);
    let answer = gh
        .call(&Call::get(
            "/repos/fixture-dev/repo/releases",
            read_of("fixture-dev/repo"),
        ))
        .expect("followed");
    assert_eq!(answer.status, 200);
    assert_eq!(
        answer.moved,
        Some((
            common::slug("fixture-dev/repo"),
            common::slug("fixture-dev/renamed")
        ))
    );
    assert_eq!(answer.requests, 3);
    assert_eq!(
        gh.meter().moved,
        Some((
            common::slug("fixture-dev/repo"),
            common::slug("fixture-dev/renamed")
        ))
    );
    let paths: Vec<String> = fake.seen().into_iter().map(|s| s.path_and_query).collect();
    assert_eq!(paths.len(), 3);
    assert_eq!(paths[0], "/repos/fixture-dev/repo/releases");
    assert!(paths[1].starts_with("/repositories/"), "{paths:?}");
    assert_eq!(paths[2], "/repos/fixture-dev/renamed/releases");
    assert_clean(&fake);
}

#[test]
fn a_truncated_body_is_truncated() {
    let fake = fake_with_repo();
    fake.fault(
        Match::api("/repos/fixture-dev/repo"),
        Fault::TruncateBody { at: 5 },
    );
    let gh = anonymous(&fake);
    let error = gh
        .call(&Call::get(
            "/repos/fixture-dev/repo",
            read_of("fixture-dev/repo"),
        ))
        .expect_err("cut short");
    assert_eq!(error.sub, Sub::Wire(WireKind::Truncated));
    assert_eq!(fake.seen().len(), 1);
    assert_clean(&fake);
}

#[test]
fn a_hang_past_the_idle_is_idle() {
    let fake = fake_with_repo();
    fake.fault(
        Match::api("/repos/fixture-dev/repo"),
        Fault::Hang(common::HANG),
    );
    let gh = anonymous(&fake);
    let error = gh
        .call(
            &Call::get("/repos/fixture-dev/repo", read_of("fixture-dev/repo"))
                .deadlines(short_idle()),
        )
        .expect_err("idle");
    assert_eq!(error.sub, Sub::Wire(WireKind::Idle));
    assert_eq!(error.kind, Kind::CouldNotRun);
    assert_clean(&fake);
}

#[test]
fn a_closed_connection_is_truncated_and_retried_to_three_requests() {
    let fake = fake_with_repo();
    fake.fault_always(Match::api("/repos/fixture-dev/repo"), Fault::Close);
    let gh = anonymous(&fake);
    let error = gh
        .call(
            &Call::get("/repos/fixture-dev/repo", read_of("fixture-dev/repo"))
                .retry(Some(RetryPolicy::STANDARD)),
        )
        .expect_err("closed every time");
    assert_eq!(error.sub, Sub::Wire(WireKind::Truncated));
    assert_eq!(fake.seen().len(), 3);
    assert_eq!(gh.meter().requests, 3);
    assert_eq!(
        gh.fx().slept(),
        [Duration::from_secs(2), Duration::from_secs(5)]
    );
    assert_clean(&fake);
}

#[test]
fn a_secondary_limit_is_waited_out_then_answered() {
    let fake = fake_with_repo();
    fake.fault(
        Match::api("/repos/fixture-dev/repo"),
        Fault::SecondaryLimit { retry_after: 1 },
    );
    let gh = anonymous(&fake);
    let answer = gh
        .call(
            &Call::get("/repos/fixture-dev/repo", read_of("fixture-dev/repo"))
                .retry(Some(RetryPolicy::STANDARD)),
        )
        .expect("answered after the wait");
    assert_eq!(answer.status, 200);
    assert_eq!(answer.requests, 2);
    assert_eq!(gh.fx().slept(), [Duration::from_secs(1)]);
    assert_clean(&fake);
}

#[cfg(unix)]
#[test]
fn a_spent_primary_limit_is_recorded_and_the_next_call_pre_refused() {
    use agh::config::Dirs;
    use agh::fx::Fx;
    use agh::rate::RateBook;

    let reset = 4_000_000_000;
    let fake = fake_with_repo();
    fake.fault(
        Match::api("/repos/fixture-dev/repo"),
        Fault::PrimaryExhausted { reset },
    );
    let root = common::scratch("loopback-primary");
    let book = RateBook::at(Dirs::under(root.path()).rate());
    let gh = agh::GithubBuilder::new(Router::empty())
        .transport(Arc::new(HttpTransport::direct(Arc::new(common::env()))))
        .endpoints(common::endpoints(&fake))
        .env(Arc::new(common::env()))
        .fx(Arc::new(Fx::simulated(1_760_000_000)))
        .caller("test")
        .retry(RetryPolicy::NONE)
        .rate_book(book.clone())
        .build();
    let call = Call::get("/repos/fixture-dev/repo", read_of("fixture-dev/repo"));
    let error = gh.call(&call).expect_err("the limit is spent");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::RateLimited, Sub::Primary)
    );
    assert_eq!(error.reset, Some(reset));
    let recorded = book.read("anonymous");
    assert!(
        recorded
            .iter()
            .any(|s| s.remaining == 0 && s.reset == reset),
        "{recorded:?}"
    );
    let again = gh.call(&call).expect_err("pre-refused");
    assert_eq!(
        (again.kind, again.sub.clone()),
        (Kind::RateLimited, Sub::PreRefused)
    );
    assert_eq!(fake.seen().len(), 1);
    assert_clean(&fake);
}

/// A transport-level GET of `path` on `role`'s origin of `fake`, with the headers the fake
/// requires, and no credential.
fn bare_request<'a>(
    fake: &FakeGithub,
    role: OriginRole,
    method: Method,
    path: &str,
) -> WireRequest<'a> {
    let endpoints = common::endpoints(fake);
    let mut headers = vec![("User-Agent".to_string(), "agh-test".to_string())];
    if matches!(role, OriginRole::Api | OriginRole::Uploads) {
        headers.push(("X-GitHub-Api-Version".to_string(), "2022-11-28".to_string()));
    }
    WireRequest::new(
        method,
        endpoints.url(role, path),
        headers,
        LIMIT,
        Deadlines::JSON,
    )
}

fn assert_not_sent_local(result: Result<agh::wire::WireResponse, WireError>) -> WireError {
    let error = result.expect_err("refused before any connection");
    assert_eq!(
        (error.phase, error.kind),
        (Phase::NotSent, WireKind::Local),
        "{error:?}"
    );
    error
}

#[test]
fn direct_transport_refuses_https_as_local() {
    let fake = fake_with_repo();
    let transport = HttpTransport::direct(Arc::new(common::env()));
    let url = format!(
        "https://127.0.0.1:{}/repos/fixture-dev/repo",
        fake.ports().api
    );
    let request = WireRequest::new(Method::Get, url, Vec::new(), LIMIT, Deadlines::JSON);
    assert_not_sent_local(transport.round_trip(request));
    assert!(fake.seen().is_empty());
    assert_clean(&fake);
}

#[test]
fn direct_transport_refuses_writes() {
    let fake = fake_with_repo();
    let transport = HttpTransport::direct(Arc::new(common::env()));
    for method in [Method::Post, Method::Patch, Method::Put, Method::Delete] {
        let request = bare_request(&fake, OriginRole::Api, method, "/repos/fixture-dev/repo");
        let error = assert_not_sent_local(transport.round_trip(request));
        assert!(error.detail.contains(method.as_str()), "{error:?}");
    }
    assert!(fake.seen().is_empty());
    assert_clean(&fake);
}

/// `round_trip` of a bare GET to `url` through `HttpTransport::new` over `env`, which reads
/// the proxy variables.
fn through_the_environment(env: agh::identity::MapEnv, url: &str) -> WireError {
    let transport = HttpTransport::new(Arc::new(env));
    let request = WireRequest::new(
        Method::Get,
        url.to_string(),
        Vec::new(),
        LIMIT,
        Deadlines::JSON,
    );
    transport
        .round_trip(request)
        .expect_err("a proxy that cannot carry it")
}

#[test]
fn an_unusable_proxy_variable_is_a_proxy_failure_before_any_connection() {
    // aterm-http refuses both values while it resolves the proxy, before it opens a socket,
    // so port 9 is never dialled.
    for value in ["https://proxy.invalid:1", "socks5://x"] {
        let env = common::env().with("HTTP_PROXY", value);
        let error = through_the_environment(env, "http://127.0.0.1:9/x");
        assert_eq!(
            (error.phase, error.kind),
            (Phase::NotSent, WireKind::Proxy),
            "{value}: {error:?}"
        );
        assert!(error.detail.contains("HTTP_PROXY"), "{error:?}");
    }
}

#[test]
fn a_proxy_that_refuses_connect_is_a_proxy_failure() {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let port = listener.local_addr().expect("its address").port();
    let (done, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let head = listener.accept().map(|(mut stream, _)| {
            stream
                .set_read_timeout(Some(common::HANG))
                .expect("a read timeout");
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                head.push(byte[0]);
            }
            let _ = stream.write_all(
                b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n",
            );
            head
        });
        let _ = done.send(head);
    });
    let env = common::env().with("HTTPS_PROXY", &format!("http://127.0.0.1:{port}"));
    let error = through_the_environment(env, "https://127.0.0.1:9/x");
    assert_eq!(
        (error.phase, error.kind),
        (Phase::NotSent, WireKind::Proxy),
        "{error:?}"
    );
    assert!(error.detail.contains("407"), "{error:?}");
    let head = heard
        .recv_timeout(common::HANG)
        .expect("the proxy heard the CONNECT (hang detector)")
        .expect("an accepted connection");
    assert!(head.starts_with(b"CONNECT 127.0.0.1:9 "), "{head:?}");
}

#[test]
fn direct_transport_sends_a_credential_only_to_its_origin() {
    let fake = fake_with_repo();
    let token = agh_fake::token("dev");
    fake.add_token(&token, "fixture-dev-bot");
    let endpoints = common::endpoints(&fake);
    let credential = Credential::for_tests("dev", Tier::Dev, token.as_bytes());
    let header = AuthHeader::for_origin_for_tests(&credential, &endpoints, endpoints.api())
        .expect("a header for the api origin");
    let transport = HttpTransport::direct(Arc::new(common::env()));

    let mut request = bare_request(
        &fake,
        OriginRole::Api,
        Method::Get,
        "/repos/fixture-dev/repo",
    );
    request.auth = Some(&header);
    let mut answer = transport.round_trip(request).expect("the api answers");
    assert_eq!(answer.status, 200);
    answer.read_body(LIMIT).expect("the body");
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].had_authorization);
    assert_eq!(seen[0].as_login.as_deref(), Some("fixture-dev-bot"));

    for role in [OriginRole::Web, OriginRole::AssetCdn] {
        let mut request = bare_request(&fake, role, Method::Get, "/fixture-dev/repo");
        request.auth = Some(&header);
        let error = assert_not_sent_local(transport.round_trip(request));
        assert!(error.detail.contains(&endpoints.api().base()), "{error:?}");
        assert!(
            error.detail.contains(&endpoints.origin(role).base()),
            "{error:?}"
        );
        assert!(!error.detail.contains(&token), "{error:?}");
        assert!(!error.detail.contains(&token[4..]), "{error:?}");
    }
    let mut request = bare_request(&fake, OriginRole::Api, Method::Get, "/");
    request.url = "ftp://127.0.0.1/x".to_string();
    request.auth = Some(&header);
    let error = assert_not_sent_local(transport.round_trip(request));
    assert!(error.detail.contains(&endpoints.api().base()), "{error:?}");
    assert!(!error.detail.contains(&token), "{error:?}");
    assert!(!error.detail.contains(&token[4..]), "{error:?}");
    assert_eq!(fake.seen().len(), 1);
    assert_clean(&fake);
}

/// The routed cases: the canonical config with the dev token file (unix, plan §1 rule 12).
#[cfg(unix)]
mod routed {
    use super::*;

    #[test]
    fn a_routed_asset_carries_the_credential_to_the_api_origin_only() {
        let fake = FakeGithub::start();
        fake.add_repo(
            "fixture-dev/repo",
            RepoSpec::private().push("fixture-dev-bot"),
        );
        let id = one_asset(&fake, "fixture-dev/repo");
        let root = common::scratch("loopback-routed");
        common::canonical_tokens(root.path(), &fake, true, false);
        let router = common::router(&common::canonical_config(root.path()), root.path());
        let gh = common::github(router, &fake);
        let mut streamed = gh
            .call_stream(&asset_call("fixture-dev/repo", id))
            .expect("the asset");
        let mut bytes = Vec::new();
        streamed.body.read_to_end(&mut bytes).expect("the bytes");
        assert_eq!(bytes, b"the asset bytes");
        assert!(streamed.hopped);
        let seen = fake.seen();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].origin, FakeOrigin::Api);
        assert!(seen[0].had_authorization);
        assert_eq!(seen[0].as_login.as_deref(), Some("fixture-dev-bot"));
        assert_eq!(seen[1].origin, FakeOrigin::Cdn);
        assert!(!seen[1].had_authorization);
        assert_clean(&fake);
    }
}
