// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `should_retry` and the pipeline's retries (the plan's §4.6.15, §8), on the simulated clock.

use std::sync::Arc;
use std::time::Duration;

use agh::policy::{RouteRequest, RouteTarget};
use agh::retry::{Idempotency, RetryPolicy, should_retry};
use agh::wire::scripted::{Scripted, ScriptedTransport};
use agh::wire::{Phase, WireError, WireKind};
use agh::{Call, Error, Kind, Sub};

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

fn wire_error() -> Error {
    Error::could_not_run("x").with_sub(Sub::Wire(WireKind::Reset))
}

fn secondary(retry_after: Option<u64>) -> Error {
    let error = Error::new(Kind::RateLimited, "x").with_sub(Sub::Secondary);
    match retry_after {
        Some(secs) => error.with_retry_after(secs),
        None => error,
    }
}

#[test]
fn should_retry_stops_at_the_attempt_budget() {
    let p = RetryPolicy::STANDARD;
    assert_eq!(
        should_retry(&p, Idempotency::Read, 3, &server(), false),
        None
    );
    assert_eq!(
        should_retry(&p, Idempotency::Read, 4, &server(), false),
        None
    );
    assert_eq!(
        should_retry(&RetryPolicy::NONE, Idempotency::Read, 1, &server(), false),
        None
    );
}

#[test]
fn should_retry_never_repeats_a_one_shot() {
    let p = RetryPolicy::STANDARD;
    assert_eq!(
        should_retry(&p, Idempotency::OneShot, 1, &server(), false),
        None
    );
    assert_eq!(
        should_retry(&p, Idempotency::OneShot, 1, &wire_error(), false),
        None
    );
}

#[test]
fn should_retry_waits_out_one_secondary_limit() {
    let p = RetryPolicy::STANDARD;
    assert_eq!(
        should_retry(&p, Idempotency::Read, 1, &secondary(Some(1)), false),
        Some(Duration::from_secs(1))
    );
    assert_eq!(
        should_retry(&p, Idempotency::Read, 1, &secondary(Some(60)), false),
        Some(Duration::from_secs(60))
    );
    assert_eq!(
        should_retry(&p, Idempotency::Read, 1, &secondary(None), false),
        Some(Duration::from_secs(60))
    );
    assert_eq!(
        should_retry(&p, Idempotency::Read, 1, &secondary(Some(61)), false),
        None
    );
    assert_eq!(
        should_retry(&p, Idempotency::Read, 1, &secondary(Some(1)), true),
        None
    );
    assert_eq!(
        should_retry(&p, Idempotency::Converging, 1, &secondary(Some(5)), false),
        Some(Duration::from_secs(5))
    );
}

#[test]
fn should_retry_never_sleeps_on_a_primary_limit_or_a_pre_refusal() {
    let p = RetryPolicy::STANDARD;
    for sub in [Sub::Primary, Sub::PreRefused] {
        let error = Error::new(Kind::RateLimited, "x").with_sub(sub);
        assert_eq!(should_retry(&p, Idempotency::Read, 1, &error, false), None);
    }
}

#[test]
fn should_retry_pauses_on_server_and_wire_failures() {
    let p = RetryPolicy::STANDARD;
    for error in [server(), wire_error()] {
        assert_eq!(
            should_retry(&p, Idempotency::Read, 1, &error, false),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            should_retry(&p, Idempotency::Read, 2, &error, false),
            Some(Duration::from_secs(5))
        );
    }
    let long = RetryPolicy {
        attempts: 5,
        ..RetryPolicy::STANDARD
    };
    assert_eq!(
        should_retry(&long, Idempotency::Read, 4, &server(), false),
        Some(Duration::from_secs(5))
    );
}

#[test]
fn should_retry_refuses_everything_else() {
    let p = RetryPolicy::STANDARD;
    let others = [
        Error::new(Kind::Unauthenticated, "x"),
        Error::new(Kind::NotFound, "x"),
        Error::new(Kind::Forbidden, "x"),
        Error::new(Kind::Invalid, "x"),
        Error::new(Kind::Conflict, "x"),
        Error::could_not_run("x").with_sub(Sub::Protocol),
        Error::could_not_run("x").with_sub(Sub::Local),
    ];
    for error in others {
        assert_eq!(
            should_retry(&p, Idempotency::Read, 1, &error, false),
            None,
            "{error}"
        );
    }
}

#[test]
fn two_server_errors_then_success_is_three_requests() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(500, &[], b"{}");
    transport.answer(500, &[], b"{}");
    transport.answer(200, &[], b"{\"id\":1}");
    let gh = common::scripted(&transport, API);
    let answered = gh.call(&standard_get()).expect("the third attempt answers");
    assert_eq!(answered.requests, 3);
    assert_eq!(transport.requests().len(), 3);
    assert_eq!(
        gh.fx().slept(),
        vec![Duration::from_secs(2), Duration::from_secs(5)]
    );
}

#[test]
fn three_server_errors_are_the_error() {
    let transport = Arc::new(ScriptedTransport::new());
    for _ in 0..3 {
        transport.answer(502, &[], b"{}");
    }
    let gh = common::scripted(&transport, API);
    let error = gh.call(&standard_get()).expect_err("the budget is spent");
    assert_eq!(error.kind, Kind::Server);
    assert_eq!(transport.requests().len(), 3);
}

#[test]
fn a_401_is_one_request() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(401, &[], br#"{"message":"Bad credentials"}"#);
    let gh = common::scripted(&transport, API);
    let error = gh.call(&standard_get()).expect_err("unauthenticated");
    assert_eq!(error.kind, Kind::Unauthenticated);
    assert_eq!(transport.requests().len(), 1);
    assert!(gh.fx().slept().is_empty());
}

#[test]
fn a_secondary_limit_is_waited_out_once() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(
        403,
        &[("retry-after", "1")],
        br#"{"message":"You have exceeded a secondary rate limit"}"#,
    );
    transport.answer(200, &[], b"{}");
    let gh = common::scripted(&transport, API);
    gh.call(&standard_get()).expect("the retry answers");
    assert_eq!(gh.fx().slept(), vec![Duration::from_secs(1)]);
    assert_eq!(transport.requests().len(), 2);
}

#[test]
fn a_second_secondary_limit_is_the_error() {
    let transport = Arc::new(ScriptedTransport::new());
    for _ in 0..2 {
        transport.answer(403, &[("retry-after", "1")], b"{}");
    }
    transport.answer(200, &[], b"{}");
    let gh = common::scripted(&transport, API);
    let error = gh
        .call(&standard_get())
        .expect_err("one secondary wait only");
    assert_eq!(
        (error.kind, error.sub.clone()),
        (Kind::RateLimited, Sub::Secondary)
    );
    assert_eq!(error.retry_after, Some(1));
    assert_eq!(transport.requests().len(), 2);
    assert_eq!(transport.remaining(), 1);
}

#[test]
fn a_wire_failure_is_retried() {
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
    assert_eq!(gh.fx().slept(), vec![Duration::from_secs(2)]);
}

#[test]
fn a_one_shot_failing_at_sent_is_never_retried() {
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
    assert!(gh.fx().slept().is_empty());
}

#[test]
fn a_probe_s_server_error_is_retried_as_the_call_s() {
    let transport = Arc::new(ScriptedTransport::new());
    transport.answer(404, &[], b"{}");
    transport.answer(503, &[], b"{}");
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
    assert_eq!(paths.len(), 4);
    assert!(paths[1].ends_with("/repos/fixture-dev/repo"), "{paths:?}");
    assert!(paths[2].ends_with("/releases/7"), "{paths:?}");
    assert_eq!(gh.fx().slept(), vec![Duration::from_secs(2)]);
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
}
