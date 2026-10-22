// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The answer and wire-failure table (the plan's §4.6.18): each status's kind and sub.

use agh::classify::{answer, wire};
use agh::error::{FieldError, Forbid, GithubBody};
use agh::retry::{Idempotency, RetryPolicy, should_retry};
use agh::wire::{Method, Phase, WireError, WireKind};
use agh::{Error, Kind, SideEffect, Sub};

use crate::common;

/// A GET's classification.
fn classify(status: u16, pairs: &[(&str, &str)], body: &str) -> Error {
    answer(
        status,
        &common::headers(pairs),
        body.as_bytes(),
        Method::Get,
        Idempotency::Read,
    )
    .expect("an error")
}

/// One row of the table: the status, its headers, its body, and the kind and sub.
type Row = (
    u16,
    Vec<(&'static str, &'static str)>,
    &'static str,
    Kind,
    Sub,
);

/// The error's kind and sub.
fn kind_sub(error: &Error) -> (Kind, Sub) {
    (error.kind, error.sub.clone())
}

#[test]
fn success_and_not_modified_are_no_error() {
    for status in [200, 201, 204, 299, 304] {
        assert!(
            answer(status, &[], b"", Method::Get, Idempotency::Read).is_none(),
            "{status}"
        );
    }
}

#[test]
fn every_row_of_the_table() {
    let rows: Vec<Row> = vec![
        (
            301,
            vec![("location", "https://api.github.com/repositories/7")],
            "",
            Kind::Conflict,
            Sub::Moved {
                to: "https://api.github.com/repositories/7".into(),
            },
        ),
        (302, vec![], "", Kind::CouldNotRun, Sub::Protocol),
        (307, vec![], "", Kind::CouldNotRun, Sub::Protocol),
        (400, vec![], "", Kind::Invalid, Sub::None),
        (401, vec![], "", Kind::Unauthenticated, Sub::None),
        (
            403,
            vec![
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1760003600"),
            ],
            "",
            Kind::RateLimited,
            Sub::Primary,
        ),
        (
            403,
            vec![("retry-after", "30")],
            "",
            Kind::RateLimited,
            Sub::Secondary,
        ),
        (
            403,
            vec![],
            r#"{"message":"You have exceeded a secondary rate limit."}"#,
            Kind::RateLimited,
            Sub::Secondary,
        ),
        (
            403,
            vec![(
                "x-github-sso",
                "required; url=https://github.com/orgs/fixture-ext/sso?y=1",
            )],
            "",
            Kind::Forbidden,
            Sub::Forbid(Forbid::Sso {
                url: Some("https://github.com/orgs/fixture-ext/sso?y=1".into()),
            }),
        ),
        (
            403,
            vec![],
            r#"{"message":"Resource not accessible by personal access token"}"#,
            Kind::Forbidden,
            Sub::Forbid(Forbid::Permission { accepted: None }),
        ),
        (
            403,
            vec![("X-Accepted-GitHub-Permissions", "contents=read")],
            "",
            Kind::Forbidden,
            Sub::Forbid(Forbid::Permission {
                accepted: Some("contents=read".into()),
            }),
        ),
        (403, vec![], "", Kind::Forbidden, Sub::Forbid(Forbid::Other)),
        (404, vec![], "", Kind::NotFound, Sub::None),
        (405, vec![], "", Kind::Conflict, Sub::None),
        (409, vec![], "", Kind::Conflict, Sub::None),
        (410, vec![], "", Kind::NotFound, Sub::None),
        (
            422,
            vec![],
            r#"{"errors":[{"code":"already_exists"}]}"#,
            Kind::Conflict,
            Sub::AlreadyExists,
        ),
        (
            422,
            vec![],
            r#"{"errors":[{"code":"invalid"}]}"#,
            Kind::Invalid,
            Sub::None,
        ),
        (
            429,
            vec![("x-ratelimit-remaining", "0")],
            "",
            Kind::RateLimited,
            Sub::Primary,
        ),
        (429, vec![], "", Kind::RateLimited, Sub::Secondary),
        (
            451,
            vec![],
            "",
            Kind::Forbidden,
            Sub::Forbid(Forbid::Blocked),
        ),
        (500, vec![], "", Kind::Server, Sub::None),
        (503, vec![], "", Kind::Server, Sub::None),
        (418, vec![], "", Kind::Invalid, Sub::None),
        (402, vec![], "", Kind::Invalid, Sub::None),
        (100, vec![], "", Kind::CouldNotRun, Sub::Protocol),
        (600, vec![], "", Kind::CouldNotRun, Sub::Protocol),
    ];
    for (status, pairs, body, kind, sub) in rows {
        let error = classify(status, &pairs, body);
        assert_eq!(kind_sub(&error), (kind, sub), "{status} {pairs:?} {body}");
        let github = error.github.clone().expect("every error carries its body");
        assert_eq!(
            github,
            GithubBody::parse(body.as_bytes()).with_headers(&common::headers(&pairs)),
            "{status}"
        );
    }
}

#[test]
fn the_why_carries_the_status_and_the_message() {
    let error = classify(500, &[], r#"{"message":"boom"}"#);
    assert!(error.why.contains("500") && error.why.contains("boom"));
    let error = classify(500, &[], "not json");
    assert!(error.why.contains("500"));
}

#[test]
fn a_redirect_other_than_301_is_protocol_on_a_write_too() {
    let error = answer(
        302,
        &common::headers(&[("location", "https://x.example/y")]),
        b"",
        Method::Post,
        Idempotency::OneShot,
    )
    .expect("an error");
    assert_eq!(kind_sub(&error), (Kind::CouldNotRun, Sub::Protocol));
}

#[test]
fn rate_limits_carry_reset_and_retry_after() {
    let error = classify(
        403,
        &[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1760003600"),
        ],
        "",
    );
    assert_eq!(error.reset, Some(1_760_003_600));
    let error = classify(403, &[("retry-after", "7")], "");
    assert_eq!(error.retry_after, Some(7));
    let error = classify(429, &[("Retry-After", "9")], "");
    assert_eq!(
        (error.sub.clone(), error.retry_after),
        (Sub::Secondary, Some(9))
    );
    let error = classify(
        429,
        &[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1760009999"),
        ],
        "",
    );
    assert_eq!(
        (error.sub.clone(), error.reset),
        (Sub::Primary, Some(1_760_009_999))
    );
}

#[test]
fn the_three_permission_headers_travel_with_the_error() {
    let error = classify(
        404,
        &[
            ("X-Accepted-GitHub-Permissions", "contents=read"),
            ("X-OAuth-Scopes", "repo"),
            ("X-Accepted-OAuth-Scopes", "repo"),
        ],
        "",
    );
    let github = error.github.clone().expect("the body");
    assert_eq!(
        github.accepted_permissions.as_deref(),
        Some("contents=read")
    );
    assert_eq!(github.oauth_scopes.as_deref(), Some("repo"));
    assert_eq!(github.accepted_oauth_scopes.as_deref(), Some("repo"));
}

#[test]
fn a_missing_oauth_scope_is_named_in_next() {
    let error = classify(
        403,
        &[
            ("x-oauth-scopes", "repo"),
            ("x-accepted-oauth-scopes", "repo, admin:public_key"),
        ],
        "",
    );
    assert!(error.next.contains("admin:public_key"), "{}", error.next);
    let error = classify(
        404,
        &[
            ("x-oauth-scopes", "repo, read:org"),
            ("x-accepted-oauth-scopes", "read:org"),
        ],
        "",
    );
    assert_eq!(error.next, Kind::NotFound.next_action());
    let error = classify(
        403,
        &[
            ("x-oauth-scopes", "repo"),
            ("x-accepted-oauth-scopes", "repo"),
        ],
        "",
    );
    assert_eq!(error.next, Kind::Forbidden.next_action());
}

#[test]
fn the_gh_corpus_rows() {
    let not_found = classify(
        404,
        &[],
        r#"{"message":"Not Found","documentation_url":"https://docs.github.com/rest","status":"404"}"#,
    );
    assert_eq!(not_found.kind, Kind::NotFound);
    let server = classify(500, &[], "");
    assert_eq!(server.kind, Kind::Server);
    assert!(server.retriable());
    assert!(should_retry(&RetryPolicy::STANDARD, Idempotency::Read, 1, &server, false).is_some());
    let bad = classify(
        401,
        &[],
        r#"{"message":"Bad credentials","documentation_url":"https://docs.github.com/rest","status":"401"}"#,
    );
    assert_eq!(bad.kind, Kind::Unauthenticated);
    assert!(!bad.retriable());
    assert_eq!(
        should_retry(&RetryPolicy::STANDARD, Idempotency::Read, 1, &bad, false),
        None
    );
    let exists = classify(
        422,
        &[],
        r#"{"message":"Validation Failed","errors":[{"resource":"Release","code":"already_exists","field":"tag_name"}]}"#,
    );
    assert_eq!(kind_sub(&exists), (Kind::Conflict, Sub::AlreadyExists));
    assert_eq!(
        exists.github.clone().expect("the body").errors,
        vec![FieldError {
            resource: Some("Release".into()),
            field: Some("tag_name".into()),
            code: Some("already_exists".into()),
            message: None,
        }]
    );
    let connect = wire(
        &WireError {
            phase: Phase::NotSent,
            kind: WireKind::Connect,
            detail: "connection refused".into(),
        },
        Method::Get,
        Idempotency::Read,
    );
    assert_eq!(
        kind_sub(&connect),
        (Kind::CouldNotRun, Sub::Wire(WireKind::Connect))
    );
}

#[test]
fn wire_failures_at_each_phase() {
    let failure = |phase| WireError {
        phase,
        kind: WireKind::Reset,
        detail: "the peer reset".into(),
    };
    for phase in [Phase::NotSent, Phase::Sent, Phase::Answered] {
        let error = wire(&failure(phase), Method::Get, Idempotency::Read);
        assert_eq!(
            kind_sub(&error),
            (Kind::CouldNotRun, Sub::Wire(WireKind::Reset)),
            "{phase:?}"
        );
        assert!(error.why.starts_with("reset: "));
        let error = wire(&failure(phase), Method::Post, Idempotency::Converging);
        assert_eq!(error.kind, Kind::CouldNotRun, "{phase:?}");
    }
    let not_sent = wire(&failure(Phase::NotSent), Method::Post, Idempotency::OneShot);
    assert_eq!(not_sent.kind, Kind::CouldNotRun);
    assert_eq!(not_sent.side_effect, SideEffect::None);
    for phase in [Phase::Sent, Phase::Answered] {
        let error = wire(&failure(phase), Method::Post, Idempotency::OneShot);
        assert_eq!(error.kind, Kind::OutcomeUnknown, "{phase:?}");
        assert_eq!(error.side_effect, SideEffect::Unknown);
    }
    let read = wire(&failure(Phase::Sent), Method::Get, Idempotency::OneShot);
    assert_eq!(read.kind, Kind::CouldNotRun);
}
