// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Answer(u16);

impl std::fmt::Display for Answer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}", self.0)
    }
}

fn scripted(script: &[u16]) -> (Result<u16, RunError<Answer>>, Vec<Retry>, u32) {
    let mut next = 0usize;
    let mut retries = Vec::new();
    let mut calls = 0;
    let result = run(
        Policy::STANDARD,
        |_| {
            calls += 1;
            let status = script[next];
            next += 1;
            if status == 200 {
                Ok(status)
            } else {
                Err(Answer(status))
            }
        },
        |error| Cause::from_http(error.0, None),
        |retry| retries.push(retry),
    );
    (result, retries, calls)
}

#[test]
fn two_504s_then_200_succeeds_and_logs_both_causes() {
    let (result, retries, calls) = scripted(&[504, 504, 200]);
    assert_eq!(result, Ok(200));
    assert_eq!(calls, 3);
    assert_eq!(retries.len(), 2);
    assert!(
        retries
            .iter()
            .all(|retry| retry.cause == Cause::Server(504))
    );
    assert_eq!(retries[0].pause, Duration::from_secs(10));
    assert_eq!(retries[1].pause, Duration::from_secs(10));
}

#[test]
fn a_404_is_an_answer_and_is_not_retried() {
    let (result, retries, calls) = scripted(&[404, 200]);
    assert_eq!(result.unwrap_err().error, Answer(404));
    assert_eq!(calls, 1);
    assert!(retries.is_empty());
}

#[test]
fn endless_5xx_stops_at_and_names_the_bound() {
    let script = [503; 19];
    let (result, retries, calls) = scripted(&script);
    let error = result.unwrap_err();
    assert_eq!(calls, 19);
    assert_eq!(retries.len(), 18);
    assert!(error.exhausted);
    assert!(error.to_string().contains("19 attempts / 200s"));
}

#[test]
fn curl_56_is_transient_but_never_overrides_a_404() {
    assert_eq!(
        Cause::from_curl(Some(56), Some(504), None),
        Some(Cause::Server(504))
    );
    assert_eq!(Cause::from_curl(Some(56), None, None), Some(Cause::Reset));
    assert_eq!(Cause::from_curl(Some(56), Some(404), None), None);
}

#[test]
fn only_a_bounded_retry_after_makes_429_transient() {
    let cause = Cause::from_http(429, Some(17)).unwrap();
    let mut budget = Budget::new(Policy::STANDARD);
    let Decision::Retry(retry) = budget.after(1, cause) else {
        panic!("17 seconds is inside the Retry-After bound");
    };
    assert_eq!(retry.pause, Duration::from_secs(17));
    let Decision::Retry(retry) = budget.after(2, cause) else {
        panic!("a second Retry-After still fits the total pause bound");
    };
    assert_eq!(retry.pause, Duration::from_secs(17));
    assert_eq!(Cause::from_http(429, None), None);
    let Decision::Retry(retry) = Budget::new(Policy::STANDARD).after(
        1,
        Cause::RateLimited {
            retry_after: Duration::from_secs(120),
        },
    ) else {
        panic!("120 seconds fits the total pause bound");
    };
    assert_eq!(retry.pause, Duration::from_secs(120));
    assert_eq!(
        Budget::new(Policy::STANDARD).after(
            1,
            Cause::RateLimited {
                retry_after: Duration::from_secs(201),
            }
        ),
        Decision::Exhausted
    );
}

#[test]
fn a_composite_budget_counts_pauses_spent_before_its_anonymous_phase() {
    let mut budget = Budget::with_paused(Policy::STANDARD, Duration::from_secs(190))
        .expect("190 seconds remains inside the shared bound");
    assert_eq!(
        budget.after(2, Cause::Server(504)),
        Decision::Retry(Retry {
            attempt: 2,
            attempts: 19,
            cause: Cause::Server(504),
            pause: Duration::from_secs(10),
        })
    );
    assert_eq!(budget.after(3, Cause::Server(504)), Decision::Exhausted);
    assert!(Budget::with_paused(Policy::STANDARD, Duration::from_secs(201)).is_none());
}

fn unix_secs(year: i64, month: i64, day: i64, hour: u64, minute: u64, second: u64) -> u64 {
    u64::try_from(crate::rfc3339::days_from_civil(year, month, day))
        .unwrap()
        .checked_mul(86_400)
        .and_then(|value| value.checked_add(hour * 3_600 + minute * 60 + second))
        .unwrap()
}

#[test]
fn retry_after_accepts_delta_seconds_and_every_http_date_wire_format() {
    let now = unix_secs(2026, 11, 8, 11, 59, 43);
    assert_eq!(retry_after_delay_secs("17", None), Some(17));
    for date in [
        "Sun, 08 Nov 2026 12:00:00 GMT",
        "Sunday, 08-Nov-26 12:00:00 GMT",
        "Sun Nov  8 12:00:00 2026",
    ] {
        assert_eq!(retry_after_delay_secs(date, Some(now)), Some(17), "{date}");
    }
    assert_eq!(
        retry_after_delay_secs("Sun, 08 Nov 2026 11:00:00 GMT", Some(now)),
        Some(0),
        "a past date requests no further wait"
    );
}

#[test]
fn retry_after_rejects_dates_without_a_clock_and_malformed_dates() {
    let now = unix_secs(2026, 11, 8, 11, 59, 43);
    assert_eq!(
        retry_after_delay_secs("Sun, 08 Nov 2026 12:00:00 GMT", None),
        None
    );
    for invalid in [
        "Sun, 31 Feb 2026 12:00:00 GMT",
        "Sun, 08 Nov 2026 24:00:00 GMT",
        "Sun, 08 Nov 2026 12:00:00 PST",
        "17 seconds",
        "",
    ] {
        assert_eq!(
            retry_after_delay_secs(invalid, Some(now)),
            None,
            "{invalid}"
        );
    }
}

#[test]
fn git_diagnostics_admit_only_exact_transport_evidence() {
    assert_eq!(
        Cause::from_git_diagnostic("error: RPC failed; HTTP 504 curl 22", None),
        Some(Cause::Server(504))
    );
    assert_eq!(
        Cause::from_git_diagnostic(
            "fatal: unable to access 'https://github.com/o/r.git/': The requested URL returned error: 404\nerror: RPC failed; curl 56 Recv failure: Connection reset by peer",
            None,
        ),
        None,
        "an answered 404 must outrank a later receive diagnostic"
    );
    assert_eq!(
        Cause::from_git_diagnostic(
            "fatal: unable to access 'https://github.com/o/r.git/': The requested URL returned error: 429",
            Some(7),
        ),
        Some(Cause::RateLimited {
            retry_after: Duration::from_secs(7)
        })
    );
    assert_eq!(
        Cause::from_git_diagnostic(
            "error: RPC failed; curl 56 Recv failure: Connection reset by peer",
            None,
        ),
        Some(Cause::Reset)
    );
    assert_eq!(
        Cause::from_git_diagnostic(
            "error: RPC failed; curl 28 Operation timed out after 1000 milliseconds",
            None,
        ),
        Some(Cause::Timeout)
    );
    assert_eq!(
        Cause::from_git_diagnostic(
            "fatal: unable to access 'https://github.com/o/r.git/': Operation too slow. Less than 1 bytes/sec transferred the last 60 seconds",
            None,
        ),
        Some(Cause::Timeout)
    );
    assert_eq!(
        Cause::from_git_diagnostic(
            "fatal: unable to access 'https://github.com/o/r.git/': Failed to connect: Connection timed out",
            None,
        ),
        Some(Cause::Timeout)
    );
    assert_eq!(
        Cause::from_git_diagnostic("remote: error: RPC failed; HTTP 504 curl 22", None),
        None,
        "remote-controlled prose is not transport evidence"
    );
    assert_eq!(
        Cause::from_git_diagnostic(
            "remote: fatal: unable to access repository: Operation too slow",
            None,
        ),
        None,
        "remote-controlled timeout prose is not transport evidence"
    );
    assert_eq!(
        Cause::from_git_diagnostic("fatal: repository not found", None),
        None
    );
}

#[test]
fn git_trace_observation_uses_the_final_header_block_and_classifies_with_precedence() {
    let observed = GitObservation::from_trace(
        "x <= Recv header: HTTP/2 429\r\n\
         x <= Recv header: Retry-After: 7\r\n",
    );
    assert_eq!(observed.status(), Some(429));
    assert_eq!(observed.retry_after_secs(), Some(7));
    assert_eq!(
        observed.cause(
            "fatal: unable to access 'https://github.com/o/r.git/': The requested URL returned error: 429"
        ),
        Some(Cause::RateLimited {
            retry_after: Duration::from_secs(7)
        })
    );

    let final_404 = GitObservation::from_trace(
        "x <= Recv header: HTTP/2 503\r\n\
         x <= Recv header: Retry-After: 9\r\n\
         x <= Recv header: HTTP/2 404\r\n",
    );
    assert_eq!(final_404.status(), Some(404));
    assert_eq!(final_404.retry_after_secs(), None);
    assert_eq!(
        final_404.cause("error: RPC failed; curl 56 Recv failure: Connection reset by peer"),
        None,
        "the observed final 404 must outrank a receive diagnostic"
    );

    let now = unix_secs(2026, 11, 8, 11, 59, 43);
    let dated = GitObservation::from_trace_at(
        "x <= Recv header: HTTP/2 429\r\n\
         x <= Recv header: Retry-After: Sun, 08 Nov 2026 12:00:00 GMT\r\n",
        Some(now),
    );
    assert_eq!(dated.retry_after_secs(), Some(17));
    assert_eq!(
        dated.cause(
            "fatal: unable to access 'https://github.com/o/r.git/': The requested URL returned error: 429"
        ),
        Some(Cause::RateLimited {
            retry_after: Duration::from_secs(17)
        })
    );
}
