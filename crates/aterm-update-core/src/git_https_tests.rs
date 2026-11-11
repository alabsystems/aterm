// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;
use std::cell::Cell;

fn diagnostic(status: u16) -> Failure {
    Failure::diagnostic(format!(
        "fatal: unable to access 'https://github.com/o/r.git/': The requested URL returned error: {status}"
    ))
}

#[test]
fn two_504s_then_200_succeeds_and_a_404_is_final() {
    let calls = Cell::new(0u32);
    let mut retries = Vec::new();
    let answer = read(
        true,
        |attempt, _| {
            calls.set(calls.get() + 1);
            if attempt < 3 {
                Err(diagnostic(504))
            } else {
                Ok(200)
            }
        },
        |retry| retries.push(retry),
    );
    assert_eq!(answer, Ok(200));
    assert_eq!(calls.get(), 3);
    assert_eq!(retries.len(), 2);

    calls.set(0);
    let absent = read(
        true,
        |_, _| {
            calls.set(calls.get() + 1);
            Err::<(), _>(diagnostic(404))
        },
        |_| panic!("404 must not retry"),
    )
    .unwrap_err();
    assert_eq!(calls.get(), 1);
    assert!(!absent.exhausted);
}

#[test]
fn endless_5xx_stops_at_the_named_bound() {
    let calls = Cell::new(0u32);
    let refused = read(
        true,
        |_, _| {
            calls.set(calls.get() + 1);
            Err::<(), _>(diagnostic(503))
        },
        |_| {},
    )
    .unwrap_err();
    assert_eq!(calls.get(), Policy::STANDARD.attempts());
    assert!(refused.exhausted);
    assert!(refused.to_string().contains("19 attempts / 200s"));
}

#[test]
fn only_exact_transport_evidence_retries_and_trace_preserves_429() {
    let calls = Cell::new(0u32);
    let arbitrary = read(
        true,
        |_, _| {
            calls.set(calls.get() + 1);
            Err::<(), _>(Failure::diagnostic("remote: HTTP 504; try again"))
        },
        |_| panic!("remote-controlled prose must not retry"),
    )
    .unwrap_err();
    assert_eq!(calls.get(), 1);
    assert!(!arbitrary.exhausted);

    let mut retries = Vec::new();
    let answer = read(
        true,
        |attempt, trace| {
            if attempt == 1 {
                std::fs::write(
                    trace.path().unwrap(),
                    "x <= Recv header: HTTP/2 429\nx <= Recv header: Retry-After: 7\n",
                )
                .unwrap();
                Err(Failure::diagnostic(
                    "fatal: unable to access 'https://github.com/o/r.git/': The requested URL returned error: 429",
                ))
            } else {
                Ok(200)
            }
        },
        |retry| retries.push(retry),
    );
    assert_eq!(answer, Ok(200));
    assert_eq!(retries.len(), 1);
    assert_eq!(
        retries[0].cause,
        Cause::RateLimited {
            retry_after: std::time::Duration::from_secs(7)
        }
    );
}

#[test]
fn curl_56_git_diagnostic_is_transient_and_non_https_is_one_shot() {
    let mut retries = Vec::new();
    let answer = read(
        true,
        |attempt, _| {
            if attempt == 1 {
                Err(Failure::diagnostic(
                    "error: RPC failed; curl 56 Recv failure: Connection reset by peer",
                ))
            } else {
                Ok(200)
            }
        },
        |retry| retries.push(retry),
    );
    assert_eq!(answer, Ok(200));
    assert_eq!(retries.len(), 1);
    assert_eq!(retries[0].cause, Cause::Reset);

    let calls = Cell::new(0u32);
    let one = read(
        false,
        |_, trace| {
            calls.set(calls.get() + 1);
            assert!(trace.path().is_none());
            Err::<(), _>(diagnostic(504))
        },
        |_| panic!("a non-anonymous transport is one-shot"),
    )
    .unwrap_err();
    assert_eq!(calls.get(), 1);
    assert!(!one.exhausted);
}

#[test]
fn anonymous_url_shape_accepts_scheme_case_but_refuses_credential_shapes() {
    assert!(is_anonymous_https_url("https://github.com/o/r.git"));
    assert!(is_anonymous_https_url("HTTPS://github.com/o/r.git"));
    for url in [
        "http://github.com/o/r.git",
        "git@github.com:o/r.git",
        "https://token@github.com/o/r.git",
        "https://github.com/o/r.git?token=secret",
        "https://github.com/o/r.git#token=secret",
        "https://github.com\\token@elsewhere/o/r.git",
        "https://github.com/o/r.git\n",
    ] {
        assert!(!is_anonymous_https_url(url), "{url:?}");
    }
}

#[test]
fn anonymous_git_environment_is_redacted_noninteractive_and_progress_bounded() {
    read(
        true,
        |_, trace| {
            let environment: std::collections::BTreeMap<_, _> =
                trace.environment().into_iter().collect();
            for (name, value) in [
                ("GIT_TRACE_CURL_NO_DATA", "1"),
                ("GIT_TRACE_REDACT", "1"),
                ("GIT_HTTP_LOW_SPEED_LIMIT", "1"),
                ("GIT_HTTP_LOW_SPEED_TIME", "60"),
                ("GIT_TERMINAL_PROMPT", "0"),
            ] {
                assert_eq!(environment.get(name).map(String::as_str), Some(value));
            }
            Ok::<_, Failure>(())
        },
        |_| unreachable!("a successful observation does not retry"),
    )
    .unwrap();
}

#[test]
fn anonymous_read_refuses_before_transport_when_private_trace_cannot_be_made() {
    let calls = Cell::new(0u32);
    let refused = read_with_scratch(
        true,
        || Err(std::io::Error::other("scratch denied")),
        |_, _| {
            calls.set(calls.get() + 1);
            Ok::<_, Failure>(())
        },
        |_| panic!("a local setup failure must not retry"),
    )
    .unwrap_err();
    assert_eq!(calls.get(), 0);
    assert_eq!(refused.attempts, 0);
    assert!(!refused.exhausted);
    assert!(refused.to_string().contains("scratch denied"), "{refused}");
}
