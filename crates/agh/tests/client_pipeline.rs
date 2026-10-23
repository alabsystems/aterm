// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Tests for the `Github` client pipeline over a scripted transport.

use std::sync::Arc;

use agh::error::{HttpRef, IdentityRef, Tried};
use agh::fx::canon::KEPT_HEADERS;
use agh::policy::{RefusalCode, RouteRequest, RouteTarget, Router};
use agh::retry::RetryPolicy;
use agh::wire::scripted::{ScriptedTransport, SeenRequest};
use agh::wire::{Method, OriginRole};
use agh::{Call, Github, Kind, Sub};

use crate::common;

const API: u16 = 47_401;

fn repo_call(path: &str) -> Call {
    Call::get(
        path,
        RouteRequest::read(RouteTarget::Repo(common::slug("fixture-dev/repo"))),
    )
}

fn header_names(seen: &SeenRequest) -> Vec<&str> {
    seen.headers.iter().map(|(n, _)| n.as_str()).collect()
}

const WRITES: [Method; 4] = [Method::Post, Method::Patch, Method::Put, Method::Delete];

fn assert_send_sync_clone<T: Send + Sync + Clone>() {}

#[test]
fn github_is_send_sync_and_clone() {
    assert_send_sync_clone::<Github>();
}

#[test]
fn headers_go_in_order_at_the_api_origin() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(200, &[], b"{}");
    let gh = common::scripted(&transport, API);
    let call = repo_call("/repos/fixture-dev/repo")
        .query("a", "x y")
        .if_none_match(Some("W/\"e\"".into()));
    gh.call(&call).expect("an answer");
    let seen = transport.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, Method::Get);
    assert_eq!(
        seen[0].url,
        format!("{}/repos/fixture-dev/repo?a=x%20y", common::base(API))
    );
    assert_eq!(
        seen[0].headers,
        vec![
            (
                "Accept".to_string(),
                "application/vnd.github+json".to_string()
            ),
            (
                "User-Agent".to_string(),
                format!("agh/{} (test)", agh::VERSION)
            ),
            ("X-GitHub-Api-Version".to_string(), "2022-11-28".to_string()),
            ("If-None-Match".to_string(), "W/\"e\"".to_string()),
        ]
    );
    assert_eq!(seen[0].auth_profile, None);
}

#[test]
fn the_web_origin_gets_no_api_version_and_the_uploads_origin_does() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(200, &[], b"");
    transport.answer(200, &[], b"");
    let gh = common::scripted(&transport, API);
    let web = repo_call("/fixture-dev/repo/releases").origin(OriginRole::Web);
    gh.call(&web).expect("the web answers");
    let uploads = repo_call("/x").origin(OriginRole::Uploads);
    gh.call(&uploads).expect("uploads answers");
    let seen = transport.requests();
    assert_eq!(header_names(&seen[0]), ["Accept", "User-Agent"]);
    assert!(seen[0].url.starts_with("http://127.0.0.1:47403/"));
    assert_eq!(
        header_names(&seen[1]),
        ["Accept", "User-Agent", "X-GitHub-Api-Version"]
    );
}

#[test]
fn every_write_method_is_refused_before_anything_happens() {
    let transport = Arc::new(ScriptedTransport::new());
    let gh = common::scripted(&transport, API);
    for method in WRITES {
        let mut call = repo_call("/repos/fixture-dev/repo");
        call.method = method;
        for error in [
            gh.call(&call).expect_err("refused"),
            gh.call_stream(&call).expect_err("refused"),
        ] {
            assert_eq!(error.kind, Kind::Refused, "{method:?}");
            assert_eq!(error.exit(), 1);
            assert_eq!(error.sub, Sub::Policy(RefusalCode::MethodNotRead));
            assert_eq!(error.sub.slug().as_deref(), Some("policy.method_not_read"));
            assert!(error.why.contains(method.as_str()));
            assert!(error.next.contains("M2"));
            assert_eq!(error.repo.as_deref(), Some("fixture-dev/repo"));
        }
    }
    assert!(transport.requests().is_empty());
    assert_eq!(gh.meter().requests, 0);
}

#[test]
fn call_refuses_a_hop_before_anything_is_sent() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(200, &[], b"");
    let gh = common::scripted(&transport, API);
    let error = gh
        .call(&repo_call("/repos/fixture-dev/repo/releases/assets/1").hop(true))
        .expect_err("call never hops");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::CouldNotRun, Sub::Protocol)
    );
    assert!(transport.requests().is_empty());
}

#[test]
fn a_refusal_sends_nothing_and_exits_by_its_code() {
    let transport = Arc::new(ScriptedTransport::new());
    let gh = common::scripted(&transport, API);
    let route = RouteRequest::writer_read(RouteTarget::Repo(common::slug("fixture-dev/repo")));
    let error = gh
        .call(&Call::get("/repos/fixture-dev/repo/releases", route))
        .expect_err("no writer without a config");
    let Sub::Policy(code) = error.sub.clone() else {
        panic!("a policy refusal: {error:?}");
    };
    assert_eq!(error.exit(), code.kind().exit());
    assert!(transport.requests().is_empty());
    assert_eq!(gh.meter().requests, 0);
}

#[test]
fn anonymous_sends_no_credential() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(200, &[], b"{}");
    let gh = common::scripted(&transport, API);
    let call = Call::get(
        "/repos/fixture-dev/repo",
        RouteRequest::read(RouteTarget::Repo(common::slug("fixture-dev/repo")))
            .with_anonymous(true),
    );
    let answered = gh.call(&call).expect("an answer");
    assert_eq!(answered.as_, IdentityRef::Anonymous);
    assert_eq!(transport.requests()[0].auth_profile, None);
    assert!(!gh.meter().fell_to_anonymous);
}

#[test]
fn an_echoed_token_is_redacted_in_the_error() {
    let token = agh_fake::token("echo");
    let body = format!(r#"{{"message":"Bad credentials {token}"}}"#);
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(401, &[], body.as_bytes());
    let gh = common::scripted(&transport, API);
    let error = gh
        .call(&repo_call("/repos/fixture-dev/repo"))
        .expect_err("401");
    let line = error.line("repo.view", gh.redactor());
    let json = aterm_json::to_string(&error.to_json(gh.redactor())).expect("json");
    for text in [line, json, error.to_string()] {
        assert!(!text.contains(&token), "{text}");
        assert!(text.contains("[redacted]"), "{text}");
    }
}

#[test]
fn every_error_carries_the_exchange() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(
        500,
        &[
            ("X-GitHub-Request-Id", "FAKE:1"),
            ("x-ratelimit-limit", "60"),
            ("x-ratelimit-remaining", "59"),
            ("x-ratelimit-reset", "1760003600"),
        ],
        br#"{"message":"boom"}"#,
    );
    let gh = common::scripted(&transport, API);
    let error = gh
        .call(&repo_call("/repos/fixture-dev/repo/releases").query("page", "2"))
        .expect_err("500");
    assert_eq!(error.kind, Kind::Server);
    assert_eq!(
        error.http,
        Some(HttpRef {
            status: 500,
            method: "GET",
            path: "/repos/fixture-dev/repo/releases".into(),
            request_id: Some("FAKE:1".into()),
        })
    );
    assert_eq!(error.identity, Some(IdentityRef::Anonymous));
    assert_eq!(error.repo.as_deref(), Some("fixture-dev/repo"));
    assert_eq!(error.rate.as_ref().map(|r| r.remaining), Some(59));
    assert_eq!(
        error.github.as_ref().and_then(|g| g.message.as_deref()),
        Some("boom")
    );
}

#[test]
fn the_repository_s_own_404_is_not_visible() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(404, &[], br#"{"message":"Not Found"}"#);
    let gh = common::scripted(&transport, API);
    let error = gh
        .call(&repo_call("/repos/fixture-dev/repo"))
        .expect_err("404");
    assert_eq!(error.kind, Kind::NotVisible);
    assert_eq!(error.exit(), 7);
    assert_eq!(
        error.tried,
        vec![Tried::Sent {
            as_: "anonymous".into(),
            status: 404
        }]
    );
    assert!(error.next.contains("agh whoami --for fixture-dev/repo"));
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn a_404_under_a_visible_repository_is_not_found() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(404, &[], br#"{"message":"Not Found"}"#);
    transport.answer(200, &[], br#"{"id":1000}"#);
    let gh = common::scripted(&transport, API);
    let error = gh
        .call(&repo_call("/repos/fixture-dev/repo/releases/tags/v9"))
        .expect_err("404");
    assert_eq!(error.kind, Kind::NotFound);
    assert_eq!(
        error.tried,
        vec![
            Tried::Sent {
                as_: "anonymous".into(),
                status: 404
            },
            Tried::Probe {
                path: "repos/fixture-dev/repo".into(),
                status: 200
            },
        ]
    );
    assert!(error.why.contains("releases/tags/v9"));
    let seen = transport.requests();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[1].url,
        format!("{}/repos/fixture-dev/repo", common::base(API))
    );
    assert_eq!(gh.meter().requests, 2);
}

#[test]
fn a_404_under_an_invisible_repository_is_not_visible() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(404, &[], b"{}");
    transport.answer(404, &[], b"{}");
    let gh = common::scripted(&transport, API);
    let error = gh
        .call(&repo_call("/repos/fixture-dev/repo/releases/7"))
        .expect_err("404");
    assert_eq!(error.kind, Kind::NotVisible);
    assert_eq!(
        error.tried,
        vec![
            Tried::Sent {
                as_: "anonymous".into(),
                status: 404
            },
            Tried::Probe {
                path: "repos/fixture-dev/repo".into(),
                status: 404
            },
        ]
    );
    assert!(error.next.contains("agh whoami --for fixture-dev/repo"));
}

#[test]
fn the_probe_s_own_error_is_the_answer() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(404, &[], b"{}");
    transport.answer(503, &[], b"{}");
    let gh = common::scripted(&transport, API);
    let error = gh
        .call(&repo_call("/repos/fixture-dev/repo/releases/7"))
        .expect_err("404 then 503");
    assert_eq!(error.kind, Kind::Server);
    assert_eq!(transport.requests().len(), 2);
}

#[test]
fn no_probe_is_not_visible_with_one_request() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(404, &[], b"{}");
    let gh = common::scripted(&transport, API);
    let error = gh
        .call(&repo_call("/repos/fixture-dev/repo/releases/7").probe_404(false))
        .expect_err("404");
    assert_eq!(error.kind, Kind::NotVisible);
    assert_eq!(
        error.tried,
        vec![Tried::Sent {
            as_: "anonymous".into(),
            status: 404
        }]
    );
    assert!(error.why.contains("--no-probe"));
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn another_target_s_404_is_not_found() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(404, &[], b"{}");
    let gh = common::scripted(&transport, API);
    let call = Call::get(
        "/users/fixture-ext",
        RouteRequest::read(RouteTarget::Owner("fixture-ext".into())),
    );
    let error = gh.call(&call).expect_err("404");
    assert_eq!(error.kind, Kind::NotFound);
    assert!(error.tried.is_empty());
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn a_read_301_is_followed_once_and_noted() {
    let transport = Arc::new(ScriptedTransport::new());
    let location = format!("{}/repositories/1000/releases", common::base(API));
    transport.answer(301, &[("location", &location)], b"{}");
    transport.answer(
        200,
        &[],
        br#"{"id":1000,"full_name":"fixture-dev/renamed"}"#,
    );
    transport.answer(200, &[], b"[]");
    let gh = common::scripted(&transport, API);
    let answered = gh
        .call(&repo_call("/repos/fixture-dev/repo/releases"))
        .expect("followed");
    let moved = (
        common::slug("fixture-dev/repo"),
        common::slug("fixture-dev/renamed"),
    );
    assert_eq!(answered.moved.as_ref(), Some(&moved));
    assert_eq!(answered.requests, 3);
    let urls: Vec<String> = transport.requests().into_iter().map(|r| r.url).collect();
    assert_eq!(
        urls,
        vec![
            format!("{}/repos/fixture-dev/repo/releases", common::base(API)),
            format!("{}/repositories/1000", common::base(API)),
            format!("{}/repos/fixture-dev/renamed/releases", common::base(API)),
        ]
    );
    assert!(transport.requests()[1].auth_profile.is_none());
    assert_eq!(gh.meter().moved, Some(moved));
}

#[test]
fn a_moved_repository_invisible_anonymously_is_not_visible() {
    let transport = Arc::new(ScriptedTransport::new());
    let location = format!("{}/repositories/1000", common::base(API));
    transport.answer(301, &[("location", &location)], b"{}");
    transport.answer(404, &[], b"{}");
    let gh = common::scripted(&transport, API);
    let error = gh
        .call(&repo_call("/repos/fixture-dev/repo"))
        .expect_err("hidden");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (
            Kind::NotVisible,
            Sub::Moved {
                to: "repositories/1000".into()
            }
        )
    );
    assert!(error.next.contains("-R"));
    assert_eq!(transport.requests().len(), 2);
}

#[test]
fn a_failed_move_lookup_names_the_repository() {
    let location = format!("{}/repositories/7", common::base(API));
    for (status, body) in [(500, &b"{}"[..]), (200, &b"not json"[..])] {
        let transport = Arc::new(ScriptedTransport::new());
        transport.answer(301, &[("location", &location)], b"{}");
        transport.answer(status, &[], body);
        let gh = common::scripted(&transport, API);
        let call = repo_call("/repos/fixture-dev/repo/releases").retry(Some(RetryPolicy::NONE));
        let error = gh.call(&call).expect_err("the lookup fails");
        assert_eq!(error.repo.as_deref(), Some("fixture-dev/repo"), "{status}");
        assert_eq!(transport.requests().len(), 2, "{status}");
    }
}

#[test]
fn a_301_elsewhere_or_unfollowed_is_conflict_moved() {
    for (location, follow) in [
        ("https://example.invalid/somewhere".to_string(), true),
        (format!("{}/repositories/1000", common::base(API)), false),
    ] {
        let transport = Arc::new(ScriptedTransport::new());
        transport.answer(301, &[("location", &location)], b"{}");
        let gh = common::scripted(&transport, API);
        let error = gh
            .call(&repo_call("/repos/fixture-dev/repo").follow_moved(follow))
            .expect_err("moved");
        assert_eq!(
            (error.kind, error.sub.clone()),
            (
                Kind::Conflict,
                Sub::Moved {
                    to: location.clone()
                }
            )
        );
        assert_eq!(transport.requests().len(), 1);
    }
}

#[test]
fn another_redirect_without_a_hop_is_protocol() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(302, &[("location", "http://127.0.0.1:47404/a")], b"");
    let gh = common::scripted(&transport, API);
    let error = gh
        .call(&repo_call("/repos/fixture-dev/repo/releases/assets/1"))
        .expect_err("302");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::CouldNotRun, Sub::Protocol)
    );
    assert!(error.why.contains("302"));
}

#[test]
fn the_id_memo_sends_once() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(200, &[], br#"{"id":1000}"#);
    transport.answer(200, &[], br#"{"id":1000}"#);
    let gh = common::scripted(&transport, API);
    let s = common::slug("fixture-dev/repo");
    let route = RouteRequest::read(RouteTarget::Repo(s.clone()));
    assert_eq!(gh.repo_id(&s, &route).expect("an id"), 1000);
    assert_eq!(
        gh.repo_id(&common::slug("Fixture-Dev/Repo"), &route)
            .expect("memo"),
        1000
    );
    assert_eq!(transport.requests().len(), 1);
    let other = gh.with_router(Router::empty());
    assert_eq!(other.repo_id(&s, &route).expect("a fresh memo"), 1000);
    assert_eq!(transport.requests().len(), 2);
}

#[test]
fn an_answer_without_an_id_is_protocol() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(200, &[], br#"{"name":"repo"}"#);
    let gh = common::scripted(&transport, API);
    let s = common::slug("fixture-dev/repo");
    let error = gh
        .repo_id(&s, &RouteRequest::read(RouteTarget::Repo(s.clone())))
        .expect_err("no id");
    assert_eq!(error.sub, Sub::Protocol);
    assert_eq!(error.repo.as_deref(), Some("fixture-dev/repo"));
}

#[test]
fn an_id_answer_that_is_not_json_names_the_repository() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(200, &[], b"not json");
    let gh = common::scripted(&transport, API);
    let s = common::slug("fixture-dev/repo");
    let error = gh
        .repo_id(&s, &RouteRequest::read(RouteTarget::Repo(s.clone())))
        .expect_err("not JSON");
    assert_eq!(error.kind, Kind::CouldNotRun);
    assert_eq!(error.repo.as_deref(), Some("fixture-dev/repo"));
}

fn asset_stream() -> Call {
    repo_call("/repos/fixture-dev/repo/releases/assets/1")
        .accept(agh::Accept::OctetStream)
        .hop(true)
}

#[test]
fn a_hop_answered_304_is_a_second_redirect() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(302, &[("location", "http://127.0.0.1:47404/a")], b"");
    transport.answer(304, &[], b"");
    let gh = common::scripted(&transport, API);
    let error = gh.call_stream(&asset_stream()).expect_err("a 304 hop");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::CouldNotRun, Sub::Protocol)
    );
    assert_eq!(error.http.as_ref().map(|h| h.status), Some(304));
    assert_eq!(transport.requests().len(), 2);
}

#[test]
fn a_stream_answered_304_is_no_streamed() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(304, &[], b"");
    let gh = common::scripted(&transport, API);
    let error = gh
        .call_stream(&repo_call("/repos/fixture-dev/repo/releases/assets/1"))
        .expect_err("a Streamed is a 2xx");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::CouldNotRun, Sub::Protocol)
    );
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn a_refused_hop_keeps_the_redirect_s_exchange() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(
        302,
        &[
            ("location", "http://elsewhere.invalid/a"),
            ("x-github-request-id", "FAKE:302"),
        ],
        b"",
    );
    let gh = common::scripted(&transport, API);
    let error = gh
        .call_stream(&asset_stream())
        .expect_err("off the asset hosts");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::CouldNotRun, Sub::Protocol)
    );
    assert!(error.why.contains("elsewhere.invalid"), "{}", error.why);
    let http = error.http.as_ref().expect("the redirect's exchange");
    assert_eq!(http.status, 302);
    assert_eq!(http.request_id.as_deref(), Some("FAKE:302"));
    assert_eq!(error.repo.as_deref(), Some("fixture-dev/repo"));
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn a_streamed_body_is_held_to_the_call_limit() {
    use std::io::Read as _;
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(200, &[], b"bytes");
    transport.answer(200, &[], b"bytes");
    let gh = common::scripted(&transport, API);
    let call = repo_call("/repos/fixture-dev/repo/releases/assets/1");
    let mut over = gh.call_stream(&call.clone().limit(4)).expect("200");
    let mut body = Vec::new();
    let error = over
        .body
        .read_to_end(&mut body)
        .expect_err("past the limit");
    assert_eq!(
        agh::wire::wire_kind_of(&error),
        agh::wire::WireKind::TooLarge
    );
    let mut exact = gh.call_stream(&call.limit(5)).expect("200");
    let mut body = Vec::new();
    exact.body.read_to_end(&mut body).expect("within the limit");
    assert_eq!(body, b"bytes");
}

#[test]
fn a_whole_read_under_a_small_limit_still_classifies_a_failure() {
    let transport = Arc::new(ScriptedTransport::new());
    let long = format!(
        r#"{{"message":"Not Found","documentation_url":"{}"}}"#,
        "x".repeat(80)
    );
    assert!(long.len() > 100);
    transport.answer(404, &[], long.as_bytes());
    transport.answer(
        200,
        &[
            ("x-github-request-id", "FAKE:150"),
            ("x-ratelimit-limit", "60"),
            ("x-ratelimit-remaining", "57"),
            ("x-ratelimit-reset", "1760003600"),
        ],
        long.as_bytes(),
    );
    transport.answer(200, &[], &[b'a'; 100]);
    let gh = common::scripted(&transport, API);
    let call = Call::get("/x", RouteRequest::read(RouteTarget::Unscoped)).limit(100);
    let error = gh.call(&call).expect_err("404");
    assert_eq!(error.kind, Kind::NotFound);
    assert_eq!(error.http.as_ref().map(|h| h.status), Some(404));
    assert_eq!(
        error.github.as_ref().and_then(|g| g.message.as_deref()),
        Some("Not Found")
    );
    let error = gh.call(&call).expect_err("a 2xx over the limit");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::CouldNotRun, Sub::Wire(agh::wire::WireKind::TooLarge))
    );
    let http = error.http.as_ref().expect("the answer it refused");
    assert_eq!(
        (
            http.status,
            http.method,
            http.path.as_str(),
            http.request_id.as_deref()
        ),
        (200, "GET", "/x", Some("FAKE:150"))
    );
    assert_eq!(error.rate.as_ref().map(|r| r.remaining), Some(57));
    let answered = gh.call(&call).expect("at the limit");
    assert_eq!(answered.body.len(), 100);
    assert_eq!(transport.requests().len(), 3);
}

#[test]
fn a_whole_read_over_the_wire_floor_is_too_large_at_the_call_limit() {
    let transport = Arc::new(ScriptedTransport::new());
    let headers = [
        ("x-github-request-id", "FAKE:170"),
        ("x-ratelimit-limit", "60"),
        ("x-ratelimit-remaining", "57"),
        ("x-ratelimit-reset", "1760003600"),
    ];
    transport.answer(200, &headers, &[b'a'; 70_000]);
    transport.answer(200, &headers, &[b'a'; 65_537]);
    let gh = common::scripted(&transport, API);
    let small = Call::get("/x", RouteRequest::read(RouteTarget::Unscoped)).limit(100);
    let error = gh.call(&small).expect_err("over the floor");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::CouldNotRun, Sub::Wire(agh::wire::WireKind::TooLarge))
    );
    assert!(error.why.contains("100 bytes"), "{}", error.why);
    assert!(!error.why.contains("65536"), "{}", error.why);
    let http = error.http.as_ref().expect("the head the floor's cut kept");
    assert_eq!(
        (http.status, http.request_id.as_deref()),
        (200, Some("FAKE:170"))
    );
    assert_eq!(error.rate.as_ref().map(|r| r.remaining), Some(57));
    let at_floor = Call::get("/x", RouteRequest::read(RouteTarget::Unscoped)).limit(65_536);
    let error = gh.call(&at_floor).expect_err("one byte over");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::CouldNotRun, Sub::Wire(agh::wire::WireKind::TooLarge))
    );
    assert!(error.why.contains("65536"), "{}", error.why);
    assert_eq!(error.http.as_ref().map(|h| h.status), Some(200));
    assert_eq!(transport.requests().len(), 2);
}

#[test]
fn the_meter_counts_and_remembers() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(
        200,
        &[
            ("x-ratelimit-limit", "60"),
            ("x-ratelimit-remaining", "58"),
            ("x-ratelimit-reset", "1760003600"),
        ],
        b"{}",
    );
    transport.answer(500, &[], b"{}");
    let gh = common::scripted(&transport, API);
    assert_eq!(gh.meter(), agh::Meter::default());
    gh.call(&repo_call("/repos/fixture-dev/repo")).expect("200");
    gh.call(&repo_call("/repos/fixture-dev/repo"))
        .expect_err("500");
    let meter = gh.meter();
    assert_eq!(meter.requests, 2);
    assert_eq!(meter.as_, Some(IdentityRef::Anonymous));
    assert_eq!(
        meter.rate.as_ref().map(|r| r.line()),
        Some("core:58/60".to_string())
    );
    assert!(meter.fell_to_anonymous);
    assert!(!meter.hopped);
    let fresh = gh.with_router(Router::empty());
    assert_eq!(fresh.meter(), agh::Meter::default());
}

#[test]
fn only_kept_headers_reach_the_answer() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(
        200,
        &[
            ("X-Frame-Options", "deny"),
            ("ETag", "W/\"abc\""),
            ("X-GitHub-Request-Id", "FAKE:9"),
            ("Content-Type", "application/json"),
        ],
        b"{}",
    );
    let gh = common::scripted(&transport, API);
    let answered = gh.call(&repo_call("/repos/fixture-dev/repo")).expect("200");
    assert!(answered.header("x-frame-options").is_none());
    assert!(
        answered
            .headers
            .iter()
            .all(|(n, _)| KEPT_HEADERS.contains(&n.as_str()))
    );
    assert_eq!(answered.etag.as_deref(), Some("W/\"abc\""));
    assert_eq!(answered.request_id.as_deref(), Some("FAKE:9"));
    assert_eq!(answered.json().expect("json"), aterm_json::json!({}));
}

#[test]
fn a_stream_hands_back_the_body_unread() {
    use std::io::Read as _;
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(
        200,
        &[("content-type", "application/octet-stream")],
        b"bytes",
    );
    transport.answer(403, &[], br#"{"message":"nope"}"#);
    let gh = common::scripted(&transport, API);
    let call = repo_call("/repos/fixture-dev/repo/releases/assets/1");
    let mut streamed = gh.call_stream(&call).expect("200");
    assert_eq!(streamed.status, 200);
    assert!(!streamed.hopped);
    let mut body = Vec::new();
    streamed.body.read_to_end(&mut body).expect("the body");
    assert_eq!(body, b"bytes");
    let error = gh.call_stream(&call).expect_err("403");
    assert_eq!(error.kind, Kind::Forbidden);
    assert_eq!(
        error.github.as_ref().and_then(|g| g.message.as_deref()),
        Some("nope")
    );
}

#[test]
fn answer_any_hands_back_any_status() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(404, &[], b"");
    let gh = common::scripted(&transport, API);
    let call = Call::head(
        "/fixture-dev/repo/releases/download/v1/a.tgz",
        RouteRequest::read(RouteTarget::Repo(common::slug("fixture-dev/repo")))
            .with_anonymous(true),
    )
    .origin(OriginRole::Web)
    .answer_any(true);
    let answered = gh.call(&call).expect("any status");
    assert_eq!(answered.status, 404);
    assert_eq!(transport.requests().len(), 1);
    assert_eq!(transport.requests()[0].method, Method::Head);
}

#[cfg(unix)]
mod routed {
    use super::*;
    use agh::policy::Named;
    use agh::wire::scripted::Scripted;

    struct World {
        root: aterm_tempfile::TempDir,
    }

    impl World {
        fn new(label: &str, dev: Option<&str>, release: bool) -> World {
            let root = common::scratch(label);
            if let Some(token) = dev {
                common::write_token(&root.path().join("tokens"), "dev.token", token);
            }
            if release {
                common::write_token(
                    &root.path().join("secrets"),
                    "release.token",
                    &agh_fake::token("release"),
                );
            }
            World { root }
        }

        fn router(&self, edit: impl Fn(String) -> String) -> Router {
            let text = edit(common::canonical_config(self.root.path()));
            common::router(&text, self.root.path())
        }

        fn github(&self, transport: &Arc<ScriptedTransport>) -> Github {
            common::github_over(
                self.router(|t| t),
                transport.clone(),
                common::scripted_endpoints(API),
            )
        }
    }

    fn classic() -> String {
        agh_fake::token("dev")
    }

    #[test]
    fn no_write_reaches_routing_whatever_the_route() {
        let world = World::new("pipeline-writes", Some(&classic()), true);
        let transport = Arc::new(ScriptedTransport::new());
        let gh = world.github(&transport);
        let routes = [
            RouteRequest::read as fn(RouteTarget) -> RouteRequest,
            RouteRequest::writer_read,
            RouteRequest::write,
        ];
        for owner in ["fixture-dev/repo", "alabsystems/aterm", "fixture-ext/repo"] {
            for route in routes {
                for named in [Named::Target("channel".into()), Named::Default] {
                    for method in WRITES {
                        let request =
                            route(RouteTarget::Repo(common::slug(owner))).with_named(named.clone());
                        let mut call = Call::get(format!("/repos/{owner}"), request);
                        call.method = method;
                        let error = gh.call(&call).expect_err("refused");
                        assert_eq!(error.sub, Sub::Policy(RefusalCode::MethodNotRead));
                        assert_eq!(error.exit(), 1);
                    }
                }
            }
        }
        assert!(transport.requests().is_empty());
        assert_eq!(format!("{:?}", gh.redactor()), "Redactor(0 learned)");
    }

    #[test]
    fn raw_get_refuses_a_write_before_anything_is_sent() {
        let world = World::new("pipeline-raw-writes", Some(&classic()), true);
        let transport = Arc::new(ScriptedTransport::new());
        let gh = world.github(&transport);
        let one = agh::github::RawGet {
            method: Method::Post,
            path: "/repos/fixture-dev/repo".into(),
            query: Vec::new(),
            accept: None,
            route: RouteRequest::read(RouteTarget::Repo(common::slug("fixture-dev/repo"))),
            all: false,
            include: false,
            opts: agh::page::ListOpts::STANDARD,
        };
        let every = agh::github::RawGet {
            path: "/orgs/fixture-dev/repos".into(),
            route: RouteRequest::read(RouteTarget::Owner("fixture-dev".into())),
            all: true,
            ..one.clone()
        };
        for request in [&one, &every] {
            let error = gh.raw().get(request).expect_err("refused");
            assert_eq!(error.kind, Kind::Refused, "{}", request.path);
            assert_eq!(error.sub, Sub::Policy(RefusalCode::MethodNotRead));
            assert_eq!(error.exit(), 1);
        }
        assert!(
            transport.requests().is_empty(),
            "{:?}",
            transport.requests()
        );
        assert_eq!(gh.meter().requests, 0);
        assert_eq!(format!("{:?}", gh.redactor()), "Redactor(0 learned)");
    }

    #[test]
    fn a_hop_s_failure_is_the_anonymous_hop_s() {
        let world = World::new("pipeline-hop-failure", Some(&classic()), false);
        let transport = Arc::new(ScriptedTransport::new());
        transport.answer(302, &[("location", "http://127.0.0.1:47404/a?sig=1")], b"");
        transport.answer(403, &[], br#"{"message":"Request has expired"}"#);
        let gh = world.github(&transport);
        let error = gh.call_stream(&asset_stream()).expect_err("403 at the CDN");
        assert_eq!(error.kind, Kind::Forbidden);
        assert_eq!(error.identity, Some(IdentityRef::Anonymous));
        assert_eq!(
            error.tried,
            vec![
                Tried::Sent {
                    as_: "dev".into(),
                    status: 302
                },
                Tried::Sent {
                    as_: "anonymous".into(),
                    status: 403
                },
            ]
        );
        assert_eq!(error.http.as_ref().map(|h| h.status), Some(403));
        let seen = transport.requests();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].auth_profile.as_deref(), Some("dev"));
        assert_eq!(seen[1].auth_profile, None);
    }

    #[test]
    fn a_routed_read_carries_the_credential_at_the_api_origin_only() {
        let world = World::new("pipeline-routed", Some(&classic()), false);
        let transport = Arc::new(ScriptedTransport::new());
        transport.answer(200, &[], b"{}");
        transport.answer(200, &[], b"");
        transport.answer(500, &[], b"");
        let gh = world.github(&transport);
        let api = repo_call("/repos/fixture-dev/repo");
        let answered = gh.call(&api).expect("200");
        let web = repo_call("/fixture-dev/repo/releases").origin(OriginRole::Web);
        let web_answer = gh.call(&web).expect("200");
        let web_error = gh.call(&web).expect_err("500");
        assert_eq!(web_error.identity, Some(IdentityRef::Anonymous));
        let seen = transport.requests();
        assert_eq!(seen[0].auth_profile.as_deref(), Some("dev"));
        assert_eq!(seen[1].auth_profile, None);
        let fingerprint = agh::identity::fingerprint(classic().as_bytes());
        assert_eq!(
            answered.as_,
            IdentityRef::Profile {
                name: "dev".into(),
                fingerprint: fingerprint.clone()
            }
        );
        assert_eq!(web_answer.as_, IdentityRef::Anonymous);
        assert_eq!(gh.meter().as_, Some(IdentityRef::Anonymous));
        assert_eq!(
            seen[0].auth_fingerprint.as_deref(),
            Some(fingerprint.as_str())
        );
        assert!(!gh.meter().fell_to_anonymous);
    }

    #[test]
    fn an_anonymous_read_sends_no_credential_though_one_is_present() {
        let world = World::new("pipeline-anon", Some(&classic()), false);
        let transport = Arc::new(ScriptedTransport::new());
        transport.answer(200, &[], b"{}");
        let gh = world.github(&transport);
        let call = Call::get(
            "/repos/fixture-dev/repo",
            RouteRequest::read(RouteTarget::Repo(common::slug("fixture-dev/repo")))
                .with_anonymous(true),
        );
        assert_eq!(gh.call(&call).expect("200").as_, IdentityRef::Anonymous);
        assert_eq!(transport.requests()[0].auth_profile, None);
    }

    #[test]
    fn an_exhausted_chain_sends_nothing_and_names_what_it_tried() {
        let world = World::new("pipeline-refused", None, false);
        let transport = Arc::new(ScriptedTransport::new());
        let gh = world.github(&transport);
        let error = gh
            .call(&repo_call("/repos/fixture-dev/repo"))
            .expect_err("no dev token");
        assert_eq!(error.sub, Sub::Policy(RefusalCode::ChainExhausted));
        assert_eq!(error.exit(), RefusalCode::ChainExhausted.kind().exit());
        assert_eq!(
            error.tried,
            vec![Tried::Skipped {
                as_: "dev".into(),
                why: "absent".into()
            }]
        );
        assert!(transport.requests().is_empty());
    }

    #[test]
    fn a_401_never_falls_back_to_anonymous() {
        let world = World::new("pipeline-401", Some(&classic()), false);
        let transport = Arc::new(ScriptedTransport::new());
        transport.answer(401, &[], br#"{"message":"Bad credentials"}"#);
        transport.answer(200, &[], b"{}");
        let gh = world.github(&transport);
        let call = Call::get(
            "/repos/alabsystems/aterm",
            RouteRequest::read(RouteTarget::Repo(common::slug("alabsystems/aterm"))),
        );
        let error = gh.call(&call).expect_err("401");
        assert_eq!(error.kind, Kind::Unauthenticated);
        assert_eq!(transport.requests().len(), 1);
        assert_eq!(transport.requests()[0].auth_profile.as_deref(), Some("dev"));
    }

    #[test]
    fn a_writer_read_301_is_conflict_moved() {
        let world = World::new("pipeline-wr301", Some(&classic()), false);
        let location = format!("{}/repositories/1000", common::base(API));
        for follow in [None, Some(true)] {
            let transport = Arc::new(ScriptedTransport::new());
            transport.answer(301, &[("location", &location)], b"{}");
            transport.answer(
                200,
                &[],
                br#"{"id":1000,"full_name":"fixture-dev/renamed"}"#,
            );
            let gh = world.github(&transport);
            let mut call = Call::get(
                "/repos/fixture-dev/repo/releases",
                RouteRequest::writer_read(RouteTarget::Repo(common::slug("fixture-dev/repo"))),
            );
            if let Some(follow) = follow {
                call = call.follow_moved(follow);
            }
            let error = gh.call(&call).expect_err("never followed");
            assert_eq!(
                (error.kind, error.sub.clone()),
                (
                    Kind::Conflict,
                    Sub::Moved {
                        to: location.clone()
                    }
                ),
                "{follow:?}"
            );
            assert_eq!(transport.requests().len(), 1, "{follow:?}");
        }
    }

    fn probed_404(label: &str, token: &str) -> agh::Error {
        let world = World::new(label, Some(token), false);
        let transport = Arc::new(ScriptedTransport::new());
        transport.answer(
            404,
            &[("x-accepted-github-permissions", "contents=read")],
            br#"{"message":"Not Found"}"#,
        );
        transport.answer(200, &[], br#"{"id":1000}"#);
        let gh = world.github(&transport);
        let error = gh
            .call(&repo_call("/repos/fixture-dev/repo/contents/a.txt"))
            .expect_err("404");
        assert_eq!(transport.requests()[1].auth_profile.as_deref(), Some("dev"));
        assert!(
            error
                .line("cat", gh.redactor())
                .contains("accepted=\"contents=read\"")
        );
        error
    }

    #[test]
    fn a_fine_grained_404_says_it_may_be_a_permission() {
        let error = probed_404("pipeline-fg", &agh_fake::fine_grained_token("dev"));
        assert_eq!(error.kind, Kind::NotFound);
        assert!(error.why.contains("contents=read"), "{}", error.why);
        assert_eq!(
            error
                .github
                .as_ref()
                .and_then(|g| g.accepted_permissions.as_deref()),
            Some("contents=read")
        );
        assert_eq!(
            error.tried,
            vec![
                Tried::Sent {
                    as_: "dev".into(),
                    status: 404
                },
                Tried::Probe {
                    path: "repos/fixture-dev/repo".into(),
                    status: 200
                },
            ]
        );
    }

    #[test]
    fn a_classic_token_s_404_keeps_the_plain_why() {
        let error = probed_404("pipeline-classic", &classic());
        assert_eq!(error.kind, Kind::NotFound);
        assert!(!error.why.contains("contents=read"), "{}", error.why);
        assert!(error.why.contains("dev"));
    }

    #[test]
    fn covers_all_repos_makes_every_404_not_found_without_a_probe() {
        let world = World::new("pipeline-covers", Some(&classic()), false);
        let transport = Arc::new(ScriptedTransport::new());
        transport.answer(404, &[], b"{}");
        transport.answer(404, &[], b"{}");
        let router = world.router(|text| {
            text.replacen(
                "login = \"fixture-dev-bot\"",
                "login = \"fixture-dev-bot\"\ncovers_all_repos = true",
                1,
            )
        });
        let gh = common::github_over(router, transport.clone(), common::scripted_endpoints(API));
        for path in [
            "/repos/fixture-dev/repo",
            "/repos/fixture-dev/repo/releases/1",
        ] {
            let error = gh.call(&repo_call(path)).expect_err("404");
            assert_eq!(error.kind, Kind::NotFound, "{path}");
        }
        assert_eq!(transport.requests().len(), 2);
    }

    #[test]
    fn covers_all_repos_still_names_a_missing_scope() {
        let world = World::new("pipeline-covers-scope", Some(&classic()), false);
        let transport = Arc::new(ScriptedTransport::new());
        transport.answer(
            404,
            &[
                ("x-oauth-scopes", "public_repo"),
                ("x-accepted-oauth-scopes", "repo"),
            ],
            br#"{"message":"Not Found"}"#,
        );
        let router = world.router(|text| {
            text.replacen(
                "login = \"fixture-dev-bot\"",
                "login = \"fixture-dev-bot\"\ncovers_all_repos = true",
                1,
            )
        });
        let gh = common::github_over(router, transport.clone(), common::scripted_endpoints(API));
        let error = gh
            .call(&repo_call("/repos/fixture-dev/repo/contents/a.txt"))
            .expect_err("404");
        assert_eq!(error.kind, Kind::NotFound);
        assert_ne!(error.next, Kind::NotFound.next_action());
        assert!(error.next.contains("repo"), "{}", error.next);
        assert_eq!(transport.requests().len(), 1);
    }

    #[test]
    fn with_router_keeps_the_seam_and_starts_its_own_caches() {
        let world = World::new("pipeline-with-router", Some(&classic()), false);
        let transport = Arc::new(ScriptedTransport::new());
        transport.push(Scripted::Answer {
            status: 200,
            headers: vec![],
            body: b"{}".to_vec(),
        });
        let gh = world.github(&transport);
        assert_eq!(gh.presence("dev"), agh::policy::Presence::Present);
        gh.call(&repo_call("/repos/fixture-dev/repo")).expect("200");
        std::fs::remove_file(world.root.path().join("tokens/dev.token")).expect("rm");
        assert_eq!(gh.presence("dev"), agh::policy::Presence::Present);
        let other = gh.with_router(world.router(|t| t));
        assert_eq!(other.presence("dev"), agh::policy::Presence::Absent);
        assert_eq!(other.meter().requests, 0);
        assert_eq!(other.user_agent(), gh.user_agent());
        assert!(Arc::ptr_eq(other.fx(), gh.fx()));
        assert_eq!(format!("{:?}", other.redactor()), "Redactor(1 learned)");
    }
}
