// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The shared retry budget and the client pipeline, over an injected transport.

use std::sync::Arc;
use std::time::Duration;

use agh::policy::{RouteRequest, RouteTarget};
use agh::retry::{Idempotency, RetryDecision, RetryPolicy, should_retry, transient_cause};
use agh::wire::scripted::{Scripted, ScriptedTransport};
use agh::wire::{Phase, WireError, WireKind};
use agh::{Call, Error, Kind, Sub};
use aterm_types::http_retry::Cause;

use crate::common;

/// The scripted api origin's port.
const API: u16 = 47_101;

/// `GET /repos/fixture-dev/repo` under STANDARD.
fn standard_get() -> Call {
    let slug = common::slug("fixture-dev/repo");
    Call::get(
        "/repos/fixture-dev/repo",
        RouteRequest::read(RouteTarget::Repo(slug)),
    )
    .retry(Some(RetryPolicy::STANDARD))
}

fn server() -> Error {
    Error::new(Kind::Server, "x")
}

fn wire_error(kind: WireKind) -> Error {
    Error::could_not_run("x").with_sub(Sub::Wire(kind))
}

#[test]
fn the_shared_budget_stops_at_nineteen_attempts() {
    let mut budget = RetryPolicy::ANONYMOUS_HTTP.budget();
    for attempt in 1..19 {
        assert!(matches!(
            should_retry(&mut budget, Idempotency::Read, attempt, &server()),
            RetryDecision::Retry(retry) if retry.pause == Duration::from_secs(10)
        ));
    }
    assert_eq!(
        should_retry(&mut budget, Idempotency::Read, 19, &server()),
        RetryDecision::Exhausted {
            bound: "19 attempts / 200s total retry-pause bound".to_string()
        }
    );
}

#[test]
fn a_one_shot_never_enters_the_retry_budget() {
    let mut budget = RetryPolicy::ANONYMOUS_HTTP.budget();
    assert_eq!(
        should_retry(&mut budget, Idempotency::OneShot, 1, &server()),
        RetryDecision::Final
    );
    assert_eq!(
        should_retry(
            &mut budget,
            Idempotency::OneShot,
            1,
            &wire_error(WireKind::Reset)
        ),
        RetryDecision::Final
    );
}

#[test]
fn only_the_four_native_wire_classes_are_transient() {
    for (kind, cause) in [
        (WireKind::Timeout, Cause::Timeout),
        (WireKind::Idle, Cause::Timeout),
        (WireKind::Reset, Cause::Reset),
        (WireKind::Truncated, Cause::Truncated),
    ] {
        assert_eq!(transient_cause(&wire_error(kind)), Some(cause), "{kind:?}");
    }
    for kind in [
        WireKind::Dns,
        WireKind::Connect,
        WireKind::Proxy,
        WireKind::Tls,
        WireKind::Trust,
        WireKind::TooLarge,
        WireKind::Protocol,
        WireKind::Revoked,
        WireKind::Local,
    ] {
        assert_eq!(transient_cause(&wire_error(kind)), None, "{kind:?}");
    }
}

#[test]
fn two_504s_then_200_succeed_and_log_each_retry_cause() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(504, &[], b"{}");
    transport.answer(504, &[], b"{}");
    transport.answer(200, &[], b"{\"id\":1}");
    let gh = common::scripted(&transport, API);
    let answered = gh.call(&standard_get()).expect("the third attempt answers");
    assert_eq!(answered.requests, 3);
    assert_eq!(transport.requests().len(), 3);
    assert_eq!(
        gh.fx().slept(),
        vec![Duration::from_secs(10), Duration::from_secs(10)]
    );
    assert_eq!(
        gh.fx().retry_notes(),
        vec![
            "agh: retry attempt 1/19 failed (HTTP 504) — retrying in 10s",
            "agh: retry attempt 2/19 failed (HTTP 504) — retrying in 10s",
        ]
    );
}

#[test]
fn a_404_is_not_retried_or_logged() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(404, &[], b"{}");
    transport.answer(200, &[], b"{}");
    let gh = common::scripted(&transport, API);
    let error = gh.call(&standard_get()).expect_err("the object is absent");
    assert_eq!(error.http.as_ref().map(|http| http.status), Some(404));
    assert_eq!(transport.requests().len(), 1);
    assert_eq!(transport.remaining(), 1);
    assert!(gh.fx().slept().is_empty());
    assert!(gh.fx().retry_notes().is_empty());
}

#[test]
fn endless_5xx_stops_at_and_names_the_shared_bound() {
    let transport = Arc::new(ScriptedTransport::new());
    for _ in 0..19 {
        transport.answer(503, &[], b"{}");
    }
    transport.answer(200, &[], b"{}");
    let gh = common::scripted(&transport, API);
    let error = gh.call(&standard_get()).expect_err("the budget is spent");
    assert_eq!(error.kind, Kind::Server);
    assert!(
        error
            .why
            .contains("19 attempts / 200s total retry-pause bound"),
        "{}",
        error.why
    );
    assert_eq!(transport.requests().len(), 19);
    assert_eq!(transport.remaining(), 1);
    assert_eq!(gh.fx().retry_notes().len(), 18);
}

#[test]
fn a_429_retries_only_with_a_valid_retry_after() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(429, &[("retry-after", "1")], b"{}");
    transport.answer(200, &[], b"{}");
    let gh = common::scripted(&transport, API);
    gh.call(&standard_get()).expect("the retry answers");
    assert_eq!(transport.requests().len(), 2);
    assert_eq!(gh.fx().slept(), vec![Duration::from_secs(1)]);
    assert_eq!(
        gh.fx().retry_notes(),
        vec!["agh: retry attempt 1/19 failed (HTTP 429 Retry-After 1s) — retrying in 1s"]
    );

    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(
        429,
        &[("retry-after", "Thu, 09 Oct 2025 08:53:27 GMT")],
        b"{}",
    );
    transport.answer(200, &[], b"{}");
    let gh = common::scripted(&transport, API);
    gh.call(&standard_get())
        .expect("an HTTP-date Retry-After is admitted by the injected clock");
    assert_eq!(transport.requests().len(), 2);
    assert_eq!(gh.fx().slept(), vec![Duration::from_secs(7)]);
    assert_eq!(gh.fx().retry_notes().len(), 1);

    // NEGATIVE CONTROL: GitHub can mark the same 429 as a spent primary limit. Dropping its
    // Retry-After while retaining only X-RateLimit-Reset turns this required retry into a final
    // answer and leaves the scripted 200 unread.
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(
        429,
        &[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1760009999"),
            ("retry-after", "1"),
        ],
        b"{}",
    );
    transport.answer(200, &[], b"{}");
    let gh = common::scripted(&transport, API);
    gh.call(&standard_get())
        .expect("a primary-limit 429 still obeys its valid Retry-After");
    assert_eq!(transport.requests().len(), 2);
    assert_eq!(gh.fx().slept(), vec![Duration::from_secs(1)]);

    for retry_after in [None, Some("not an HTTP date")] {
        let transport = Arc::new(ScriptedTransport::new());
        match retry_after {
            Some(value) => transport.answer(429, &[("retry-after", value)], b"{}"),
            None => transport.answer(429, &[], b"{}"),
        }
        transport.answer(200, &[], b"{}");
        let gh = common::scripted(&transport, API);
        let error = gh
            .call(&standard_get())
            .expect_err("a valid Retry-After is required");
        assert_eq!(error.kind, Kind::RateLimited);
        assert_eq!(transport.requests().len(), 1);
        assert!(gh.fx().retry_notes().is_empty());
    }
}

#[test]
fn a_404_stays_final_even_with_a_valid_retry_after_date() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(
        404,
        &[("retry-after", "Thu, 09 Oct 2025 08:53:27 GMT")],
        b"{}",
    );
    transport.answer(200, &[], b"{}");
    let gh = common::scripted(&transport, API);
    let error = gh.call(&standard_get()).expect_err("absence is final");
    assert_eq!(error.http.as_ref().map(|http| http.status), Some(404));
    assert_eq!(transport.requests().len(), 1);
    assert_eq!(transport.remaining(), 1);
    assert!(gh.fx().retry_notes().is_empty());
}

#[test]
fn a_403_secondary_limit_is_not_the_429_retry_class() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(
        403,
        &[("retry-after", "1")],
        br#"{"message":"secondary rate limit"}"#,
    );
    transport.answer(200, &[], b"{}");
    let gh = common::scripted(&transport, API);
    let error = gh.call(&standard_get()).expect_err("403 is final");
    assert_eq!(error.kind, Kind::RateLimited);
    assert_eq!(transport.requests().len(), 1);
    assert!(gh.fx().retry_notes().is_empty());
}

#[test]
fn a_reset_is_retried_and_logged() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.push(Scripted::Fail(WireError {
        phase: Phase::Sent,
        kind: WireKind::Reset,
        detail: "reset".into(),
    }));
    transport.answer(200, &[], b"{}");
    let gh = common::scripted(&transport, API);
    let answered = gh.call(&standard_get()).expect("the retry answers");
    assert_eq!(answered.requests, 2);
    assert_eq!(gh.fx().slept(), vec![Duration::from_secs(10)]);
    assert_eq!(
        gh.fx().retry_notes(),
        vec!["agh: retry attempt 1/19 failed (connection reset (curl exit 56)) — retrying in 10s"]
    );
}

#[test]
fn a_one_shot_failing_after_send_is_never_retried() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.push(Scripted::Fail(WireError {
        phase: Phase::Sent,
        kind: WireKind::Reset,
        detail: "reset".into(),
    }));
    transport.answer(200, &[], b"{}");
    let gh = common::scripted(&transport, API);
    let mut call = standard_get();
    call.idempotency = Idempotency::OneShot;
    let error = gh.call(&call).expect_err("no retry");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::CouldNotRun, Sub::Wire(WireKind::Reset))
    );
    assert_eq!(transport.requests().len(), 1);
    assert!(gh.fx().retry_notes().is_empty());
}

#[test]
fn repository_visibility_probes_share_the_outer_calls_retry_budget() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(404, &[], b"{}");
    transport.answer(504, &[], b"{}");
    transport.answer(404, &[], b"{}");
    transport.answer(504, &[], b"{}");
    transport.answer(404, &[], b"{}");
    transport.answer(200, &[], b"{\"id\":1}");
    let gh = common::scripted(&transport, API);
    let slug = common::slug("fixture-dev/repo");
    let call = Call::get(
        "/repos/fixture-dev/repo/releases/7",
        RouteRequest::read(RouteTarget::Repo(slug)),
    )
    .retry(Some(RetryPolicy::STANDARD));
    let error = gh.call(&call).expect_err("the release is absent");
    assert_eq!(error.kind, Kind::NotFound);
    let paths: Vec<String> = transport.requests().into_iter().map(|s| s.url).collect();
    assert_eq!(paths.len(), 6);
    assert!(paths[1].ends_with("/repos/fixture-dev/repo"), "{paths:?}");
    assert!(paths[2].ends_with("/releases/7"), "{paths:?}");
    assert!(paths[3].ends_with("/repos/fixture-dev/repo"), "{paths:?}");
    assert!(paths[4].ends_with("/releases/7"), "{paths:?}");
    assert!(paths[5].ends_with("/repos/fixture-dev/repo"), "{paths:?}");
    assert_eq!(
        gh.fx().retry_notes(),
        vec![
            "agh: retry attempt 1/19 failed (HTTP 504) — retrying in 10s",
            "agh: retry attempt 2/19 failed (HTTP 504) — retrying in 10s",
        ]
    );

    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(404, &[], b"{}");
    transport.answer(404, &[], b"{}");
    transport.answer(200, &[], b"{}");
    let gh = common::scripted(&transport, API);
    let slug = common::slug("fixture-dev/repo");
    let call = Call::get(
        "/repos/fixture-dev/repo/releases/7",
        RouteRequest::read(RouteTarget::Repo(slug)),
    )
    .retry(Some(RetryPolicy::STANDARD));
    let error = gh
        .call(&call)
        .expect_err("the repository probe's 404 is final");
    assert_eq!(error.kind, Kind::NotVisible);
    assert_eq!(transport.requests().len(), 2);
    assert_eq!(transport.remaining(), 1);
    assert!(gh.fx().retry_notes().is_empty());
}

#[test]
fn the_client_policy_applies_when_the_call_names_none() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(500, &[], b"{}");
    transport.answer(200, &[], b"{}");
    let gh = common::scripted(&transport, API);
    let call = standard_get().retry(None);
    let error = gh
        .call(&call)
        .expect_err("the test client's policy is NONE");
    assert_eq!(error.kind, Kind::Server);
    assert_eq!(transport.requests().len(), 1);
    assert!(!error.why.contains("retry-pause bound"), "{}", error.why);
}

#[test]
fn public_head_retries_transient_statuses_but_not_absence() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(504, &[], b"");
    transport.answer(504, &[], b"");
    transport.answer(200, &[], b"");
    let gh = common::scripted_with_retry(&transport, API, RetryPolicy::STANDARD);
    assert_eq!(
        gh.public()
            .probe(&common::slug("fixture-dev/repo"), "v1", "a.bin"),
        agh::github::Probe::Present
    );
    assert_eq!(transport.requests().len(), 3);
    assert_eq!(gh.fx().retry_notes().len(), 2);

    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(404, &[], b"");
    transport.answer(200, &[], b"");
    let gh = common::scripted_with_retry(&transport, API, RetryPolicy::STANDARD);
    assert_eq!(
        gh.public()
            .probe(&common::slug("fixture-dev/repo"), "v1", "missing.bin"),
        agh::github::Probe::Absent
    );
    assert_eq!(transport.requests().len(), 1);
    assert_eq!(transport.remaining(), 1);
    assert!(gh.fx().retry_notes().is_empty());
}

#[test]
fn public_body_resumes_from_the_completed_prefix() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer_body_fault(200, &[], b"first ", WireKind::Reset);
    transport.answer(206, &[("Content-Range", "bytes 6-9/10")], b"body");
    let gh = common::scripted(&transport, API);
    let root = aterm_tempfile::tempdir().expect("scratch");
    let dest = root.path().join("a.bin");
    assert_eq!(
        gh.public()
            .fetch(
                &common::slug("fixture-dev/repo"),
                "v1",
                "a.bin",
                &dest,
                1024,
            )
            .expect("the resumed body"),
        10
    );
    assert_eq!(
        std::fs::read(&dest).expect("the completed file"),
        b"first body"
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1]
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("range"))
            .map(|(_, value)| value.as_str()),
        Some("bytes=6-")
    );
    assert_eq!(gh.fx().retry_notes().len(), 1);
}

/// `request`'s Range header.
fn range_of(request: &agh::wire::scripted::SeenRequest) -> Option<&str> {
    request
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("range"))
        .map(|(_, value)| value.as_str())
}

/// The web route's endless short progress stops at the progress bound, and the API asset route
/// then resumes from the 17 bytes the web route left: its request and the CDN hop both carry
/// `Range: bytes=17-`, and the file is the web prefix plus the CDN tail. Negative controls: a
/// fetch that restarted the body on the API route would send no Range; one that kept to the web
/// route would stop at the bound with nothing written.
#[test]
fn endless_short_public_progress_stops_at_the_progress_bound_and_resumes_on_the_api_route() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer_body_fault(200, &[], b"x", WireKind::Truncated);
    for offset in 1..=16 {
        let range = format!("bytes {offset}-{offset}/1024");
        transport.answer_body_fault(
            206,
            &[("Content-Range", range.as_str())],
            b"x",
            WireKind::Truncated,
        );
    }
    transport.answer(
        200,
        &[],
        br#"{"id":1,"tag_name":"v1","assets":[{"id":7,"name":"a.bin","size":1024,"state":"uploaded"}]}"#,
    );
    let cdn = format!("http://127.0.0.1:{}/assets/7", API + 3);
    transport.answer(302, &[("location", cdn.as_str())], b"");
    transport.answer(
        206,
        &[("Content-Range", "bytes 17-1023/1024")],
        &[b'y'; 1024 - 17],
    );
    let gh = common::scripted(&transport, API);
    let root = aterm_tempfile::tempdir().expect("scratch");
    let dest = root.path().join("a.bin");
    let written = gh
        .public()
        .fetch(
            &common::slug("fixture-dev/repo"),
            "v1",
            "a.bin",
            &dest,
            1024,
        )
        .expect("the API route completes the body");
    assert_eq!(written, 1024);
    let bytes = std::fs::read(&dest).expect("the completed file");
    assert!(bytes[..17].iter().all(|b| *b == b'x'));
    assert!(bytes[17..].iter().all(|b| *b == b'y'));
    let requests = transport.requests();
    assert_eq!(requests.len(), 20);
    assert!(requests[..17].iter().all(|r| {
        r.url
            .contains("/fixture-dev/repo/releases/download/v1/a.bin")
    }));
    assert!(
        requests[17]
            .url
            .ends_with("/repos/fixture-dev/repo/releases/tags/v1"),
        "{}",
        requests[17].url
    );
    assert!(
        requests[18]
            .url
            .ends_with("/repos/fixture-dev/repo/releases/assets/7"),
        "{}",
        requests[18].url
    );
    assert_eq!(requests[19].url, cdn);
    assert_eq!(range_of(&requests[18]), Some("bytes=17-"));
    assert_eq!(range_of(&requests[19]), Some("bytes=17-"));
    assert!(requests.iter().all(|r| r.auth_profile.is_none()));
    assert_eq!(gh.fx().retry_notes().len(), 16);
    assert!(gh.fx().slept().is_empty());
}

/// A round in which neither route reached a host ends the fetch at once, naming both. Negative
/// control: a round in which the API host answered (a 504) pauses and goes on to another round.
#[test]
fn a_round_that_reaches_no_host_on_either_route_ends_the_fetch_at_once() {
    let refused = || {
        Scripted::Fail(WireError {
            phase: Phase::NotSent,
            kind: WireKind::Connect,
            detail: "connection refused".into(),
        })
    };
    let fetch = |gh: &agh::Github| {
        let root = aterm_tempfile::tempdir().expect("scratch");
        gh.public()
            .fetch(
                &common::slug("fixture-dev/repo"),
                "v1",
                "a.bin",
                &root.path().join("a.bin"),
                1024,
            )
            .expect_err("nothing serves")
    };

    let transport = Arc::new(ScriptedTransport::new());
    transport.push(refused());
    transport.push(refused());
    let gh = common::scripted(&transport, API);
    let error = fetch(&gh);
    assert!(
        error.why.contains("the download host:")
            && error.why.contains("the API asset route:")
            && error.why.contains("neither route reached a host"),
        "{}",
        error.why
    );
    assert_eq!(transport.requests().len(), 2);
    assert!(gh.fx().slept().is_empty());

    let transport = Arc::new(ScriptedTransport::new());
    transport.push(refused());
    transport.answer(504, &[], b"{}");
    let gh = common::scripted(&transport, API);
    let error = fetch(&gh);
    assert!(
        error.why.contains("no answer queued"),
        "the second round's web leg ends the script: {}",
        error.why
    );
    assert_eq!(transport.requests().len(), 3);
    assert_eq!(gh.fx().slept(), vec![Duration::from_secs(2)]);
    assert_eq!(gh.fx().retry_notes().len(), 1);
}
