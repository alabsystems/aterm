// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! One bounded retry policy for idempotent anonymous HTTP reads.
//!
//! Transports classify an answer into [`Cause`] and feed it to [`Budget`]. The policy retries
//! only server failures, a valid `Retry-After` on HTTP 429, and a connection reset, truncated
//! response or timeout. Every other 4xx is a final answer. Callers own their clock and log sink,
//! so agh can tape sleeps while curl callers can use their interruptible wait.

use std::time::Duration;

/// The retry policy shared by anonymous cutter and verifier reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    attempts: u32,
    pauses: [Duration; 2],
    max_retry_after: Duration,
    max_total_pause: Duration,
}

impl Policy {
    /// Nineteen attempts with 10 s ordinary pauses; a `Retry-After` wait is admitted up to the
    /// full 200 s total pause bound.
    /// Across one read, retry sleeps total at most 200 s. This internalizes the
    /// incident-tested `retry = 18`, `retry-delay = 10`, `retry-max-time = 200` curl setting
    /// without curl's indiscriminate `retry-all-errors` classification.
    pub const STANDARD: Self = Self {
        attempts: 19,
        pauses: [Duration::from_secs(10), Duration::from_secs(10)],
        max_retry_after: Duration::from_secs(200),
        max_total_pause: Duration::from_secs(200),
    };

    /// A policy with no retry, for fixtures and explicitly one-shot observations.
    pub const NONE: Self = Self {
        attempts: 1,
        pauses: [Duration::ZERO; 2],
        max_retry_after: Duration::ZERO,
        max_total_pause: Duration::ZERO,
    };

    /// A policy from explicit bounds. `pauses[0]` follows attempt 1 and `pauses[1]` every later
    /// ordinary transient failure.
    #[must_use]
    pub const fn new(
        attempts: u32,
        pauses: [Duration; 2],
        max_retry_after: Duration,
        max_total_pause: Duration,
    ) -> Self {
        Self {
            attempts,
            pauses,
            max_retry_after,
            max_total_pause,
        }
    }

    /// The maximum number of attempts, the first included.
    #[must_use]
    pub const fn attempts(self) -> u32 {
        self.attempts
    }

    /// The total retry-sleep bound.
    #[must_use]
    pub const fn max_total_pause(self) -> Duration {
        self.max_total_pause
    }

    /// A stable description suitable for an exhausted-read refusal.
    #[must_use]
    pub fn bound(self) -> String {
        format!(
            "{} attempts / {}s total retry-pause bound",
            self.attempts,
            self.max_total_pause.as_secs()
        )
    }
}

/// Why another idempotent read can change the result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// GitHub (or its edge) answered a server-side status.
    Server(u16),
    /// HTTP 429 carried a valid `Retry-After`.
    RateLimited {
        /// The server's requested wait.
        retry_after: Duration,
    },
    /// The connection was reset before the complete answer arrived (curl 56).
    Reset,
    /// The answer ended before its declared body did (curl 18).
    Truncated,
    /// The operation or no-progress deadline expired (curl 28).
    Timeout,
}

impl Cause {
    /// Classify an HTTP answer. Every 5xx is transient. A 429 is transient only when it carries
    /// a valid `Retry-After`; 404 and every other 4xx are final answers.
    #[must_use]
    pub const fn from_http(status: u16, retry_after_secs: Option<u64>) -> Option<Self> {
        match status {
            500..=599 => Some(Self::Server(status)),
            429 => match retry_after_secs {
                Some(seconds) => Some(Self::RateLimited {
                    retry_after: Duration::from_secs(seconds),
                }),
                None => None,
            },
            _ => None,
        }
    }

    /// Classify curl's result. An answered 4xx takes precedence over curl's exit code, so an
    /// HTTP/2 receive error cannot turn a 404 into a retry. Exit 56 is a reset when no final 4xx
    /// answer exists, including curl 8.7.1's exit for an HTTP/2 5xx.
    #[must_use]
    pub const fn from_curl(
        exit: Option<i32>,
        status: Option<u16>,
        retry_after_secs: Option<u64>,
    ) -> Option<Self> {
        if let Some(status) = status {
            if let Some(cause) = Self::from_http(status, retry_after_secs) {
                return Some(cause);
            }
            if status >= 400 && status <= 499 {
                return None;
            }
        }
        match exit {
            Some(18) => Some(Self::Truncated),
            Some(28) => Some(Self::Timeout),
            Some(56) => Some(Self::Reset),
            _ => None,
        }
    }

    /// Classify the exact diagnostics Git's libcurl transport emits for a failed smart-HTTP
    /// read. Generic Git exit 128, arbitrary remote text and unrecognised diagnostics are
    /// final: only Git's own `fatal: unable to access …` and `error: RPC failed; …` lines are
    /// inspected. An explicit 4xx takes precedence over a receive failure, as in
    /// [`Self::from_curl`].
    ///
    /// Git does not normally print response headers. A caller that captured a valid
    /// `Retry-After` through a private, headers-only, redacted trace can pass it here; without
    /// that evidence, a 429 is final.
    #[must_use]
    pub fn from_git_diagnostic(diagnostic: &str, retry_after_secs: Option<u64>) -> Option<Self> {
        let mut status = None;
        let mut curl_exit = None;
        for line in diagnostic.lines().map(str::trim) {
            let own = line.starts_with("fatal: unable to access ")
                || line.starts_with("error: RPC failed;");
            if !own {
                continue;
            }
            if let Some(found) = decimal_after(line, "The requested URL returned error: ")
                .or_else(|| decimal_after(line, "error: RPC failed; HTTP "))
            {
                status = Some(found);
            }
            curl_exit = decimal_after(line, " curl ")
                .map(i32::from)
                .or(curl_exit)
                .or_else(|| {
                    if line.contains("Recv failure: Connection reset") {
                        Some(56)
                    } else if line.contains("Operation timed out")
                        || line.contains("Operation too slow")
                        || line.contains("Connection timed out")
                        || line.contains("SSL connection timeout")
                    {
                        Some(28)
                    } else if line.contains("transfer closed with") || line.contains("partial file")
                    {
                        Some(18)
                    } else {
                        None
                    }
                });
        }
        Self::from_curl(curl_exit, status, retry_after_secs)
    }
}

/// Parse a `Retry-After` field into a delay from `now_unix_secs`.
///
/// Decimal delay-seconds do not need a clock. An HTTP-date does, and may use any of the three
/// wire formats HTTP recipients have always had to accept: IMF-fixdate, RFC 850 or asctime. A
/// date in the past means no delay. Malformed fields and dates whose delay does not fit `u64`
/// are rejected; the caller's retry policy still decides whether the resulting delay is inside
/// its bound.
#[must_use]
pub fn retry_after_delay_secs(value: &str, now_unix_secs: Option<u64>) -> Option<u64> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return value.parse().ok();
    }
    let now = now_unix_secs?;
    let date = parse_http_date(value, now)?;
    let now = i128::from(now);
    if date <= now {
        Some(0)
    } else {
        u64::try_from(date - now).ok()
    }
}

fn parse_http_date(value: &str, now_unix_secs: u64) -> Option<i128> {
    if let Some((weekday, rest)) = value.split_once(", ") {
        if is_short_weekday(weekday) {
            return parse_imf_date(rest);
        }
        if is_long_weekday(weekday) {
            return parse_rfc850_date(rest, now_unix_secs);
        }
        return None;
    }
    parse_asctime_date(value)
}

fn parse_imf_date(value: &str) -> Option<i128> {
    let mut fields = value.split_ascii_whitespace();
    let day = parse_digits(fields.next()?, 2)?;
    let month = parse_month(fields.next()?)?;
    let year = parse_digits(fields.next()?, 4)?;
    let time = fields.next()?;
    let zone = fields.next()?;
    (fields.next().is_none() && zone.eq_ignore_ascii_case("GMT"))
        .then_some(())
        .and_then(|()| date_seconds(year, month, day, time))
}

fn parse_rfc850_date(value: &str, now_unix_secs: u64) -> Option<i128> {
    let mut fields = value.split_ascii_whitespace();
    let date = fields.next()?;
    let time = fields.next()?;
    let zone = fields.next()?;
    if fields.next().is_some() || !zone.eq_ignore_ascii_case("GMT") {
        return None;
    }
    let mut date_fields = date.split('-');
    let day = parse_digits(date_fields.next()?, 2)?;
    let month = parse_month(date_fields.next()?)?;
    let short_year = parse_digits(date_fields.next()?, 2)?;
    if date_fields.next().is_some() {
        return None;
    }
    let now_days = i64::try_from(now_unix_secs / 86_400).ok()?;
    let (now_year, _, _) = crate::rfc3339::civil_from_days(now_days);
    let mut year = now_year.div_euclid(100) * 100 + short_year;
    if year > now_year.saturating_add(50) {
        year = year.saturating_sub(100);
    }
    date_seconds(year, month, day, time)
}

fn parse_asctime_date(value: &str) -> Option<i128> {
    let mut fields = value.split_ascii_whitespace();
    let weekday = fields.next()?;
    let month = parse_month(fields.next()?)?;
    let day = parse_digits_in(fields.next()?, 1, 2)?;
    let time = fields.next()?;
    let year = parse_digits(fields.next()?, 4)?;
    (fields.next().is_none() && is_short_weekday(weekday))
        .then_some(())
        .and_then(|()| date_seconds(year, month, day, time))
}

fn date_seconds(year: i64, month: i64, day: i64, time: &str) -> Option<i128> {
    let mut time_fields = time.split(':');
    let hour = parse_digits(time_fields.next()?, 2)?;
    let minute = parse_digits(time_fields.next()?, 2)?;
    let second = parse_digits(time_fields.next()?, 2)?;
    if time_fields.next().is_some()
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
        || !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
    {
        return None;
    }
    let days = i128::from(crate::rfc3339::days_from_civil(year, month, day));
    Some(days * 86_400 + i128::from(hour) * 3_600 + i128::from(minute) * 60 + i128::from(second))
}

fn parse_digits(value: &str, width: usize) -> Option<i64> {
    if value.len() != width {
        return None;
    }
    parse_digits_in(value, width, width)
}

fn parse_digits_in(value: &str, min: usize, max: usize) -> Option<i64> {
    (value.len() >= min && value.len() <= max && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

const fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

fn parse_month(value: &str) -> Option<i64> {
    [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|month| value.eq_ignore_ascii_case(month))
    .map(|month| month as i64 + 1)
}

fn is_short_weekday(value: &str) -> bool {
    ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
        .iter()
        .any(|day| value.eq_ignore_ascii_case(day))
}

fn is_long_weekday(value: &str) -> bool {
    [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ]
    .iter()
    .any(|day| value.eq_ignore_ascii_case(day))
}

/// The final HTTP status and valid `Retry-After` from Git's headers-only curl trace.
///
/// Git normally omits response headers from stderr. A transport may capture the private trace
/// with `GIT_TRACE_CURL_NO_DATA=1` and `GIT_TRACE_REDACT=1`, bound how much of that file it reads,
/// then feed the text here. This parser is pure: creating, bounding and deleting the trace stays
/// with the transport that owns it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GitObservation {
    status: Option<u16>,
    retry_after_secs: Option<u64>,
}

impl GitObservation {
    /// Parse Git's received-header trace lines. Each new HTTP status starts a response block and
    /// clears an earlier block's `Retry-After`; the last response block wins.
    #[must_use]
    pub fn from_trace(trace: &str) -> Self {
        Self::from_trace_at(trace, None)
    }

    /// Parse Git's received-header trace lines at `now_unix_secs`. Supplying a clock admits an
    /// HTTP-date `Retry-After`; decimal delay-seconds are admitted with or without one.
    #[must_use]
    pub fn from_trace_at(trace: &str, now_unix_secs: Option<u64>) -> Self {
        let mut observed = Self::default();
        for line in trace.lines() {
            let Some(header) = line.split_once("<= Recv header: ").map(|(_, h)| h.trim()) else {
                continue;
            };
            if let Some(rest) = header.strip_prefix("HTTP/") {
                let mut words = rest.split_ascii_whitespace();
                let _version = words.next();
                observed.status = words.next().and_then(|word| word.parse().ok());
                observed.retry_after_secs = None;
            } else if let Some(value) = header
                .split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
                .map(|(_, value)| value.trim())
            {
                observed.retry_after_secs = retry_after_delay_secs(value, now_unix_secs);
            }
        }
        observed
    }

    /// The final status found in the trace.
    #[must_use]
    pub const fn status(self) -> Option<u16> {
        self.status
    }

    /// The final response block's `Retry-After` delay, in seconds.
    #[must_use]
    pub const fn retry_after_secs(self) -> Option<u64> {
        self.retry_after_secs
    }

    /// Classify this observation and Git's stderr together. An observed 4xx is final and
    /// outranks stderr's receive-error text; otherwise only Git's exact diagnostics are admitted.
    #[must_use]
    pub fn cause(self, diagnostic: &str) -> Option<Cause> {
        if let Some(status) = self.status {
            if let Some(cause) = Cause::from_http(status, self.retry_after_secs) {
                return Some(cause);
            }
            if (400..=499).contains(&status) {
                return None;
            }
        }
        Cause::from_git_diagnostic(diagnostic, self.retry_after_secs)
    }
}

fn decimal_after(text: &str, marker: &str) -> Option<u16> {
    let tail = text.rsplit_once(marker)?.1;
    let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
    (digits != 0).then(|| tail[..digits].parse().ok()).flatten()
}

impl std::fmt::Display for Cause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Server(status) => write!(f, "HTTP {status}"),
            Self::RateLimited { retry_after } => {
                write!(f, "HTTP 429 Retry-After {}s", retry_after.as_secs())
            }
            Self::Reset => f.write_str("connection reset (curl exit 56)"),
            Self::Truncated => f.write_str("truncated response (curl exit 18)"),
            Self::Timeout => f.write_str("connection or no-progress timeout (curl exit 28)"),
        }
    }
}

/// One retry the budget admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retry {
    /// The failed attempt.
    pub attempt: u32,
    /// The maximum number of attempts.
    pub attempts: u32,
    /// Why this failure is transient.
    pub cause: Cause,
    /// How long to wait before the next attempt.
    pub pause: Duration,
}

impl std::fmt::Display for Retry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "attempt {}/{} failed ({}) — retrying in {}s",
            self.attempt,
            self.attempts,
            self.cause,
            self.pause.as_secs()
        )
    }
}

/// The decision after a transient failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Another attempt is within the bound.
    Retry(Retry),
    /// No more attempt is within the bound.
    Exhausted,
}

/// The state of one read's retry budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    policy: Policy,
    paused: Duration,
}

impl Budget {
    /// A fresh budget for `policy`.
    #[must_use]
    pub const fn new(policy: Policy) -> Self {
        Self {
            policy,
            paused: Duration::ZERO,
        }
    }

    /// A budget for `policy` after `paused` was already spent by an earlier transport phase.
    /// Returns `None` when that earlier phase has already exceeded the policy's pause bound.
    #[must_use]
    pub fn with_paused(policy: Policy, paused: Duration) -> Option<Self> {
        (paused <= policy.max_total_pause).then_some(Self { policy, paused })
    }

    /// Decide what follows transient `cause` from `attempt` (numbered from one).
    #[must_use]
    pub fn after(&mut self, attempt: u32, cause: Cause) -> Decision {
        if attempt >= self.policy.attempts {
            return Decision::Exhausted;
        }
        let pause = match cause {
            Cause::RateLimited { retry_after } if retry_after <= self.policy.max_retry_after => {
                retry_after
            }
            Cause::RateLimited { .. } => return Decision::Exhausted,
            _ => self.policy.pauses[usize::from(attempt >= 2)],
        };
        let Some(paused) = self.paused.checked_add(pause) else {
            return Decision::Exhausted;
        };
        if paused > self.policy.max_total_pause {
            return Decision::Exhausted;
        }
        self.paused = paused;
        Decision::Retry(Retry {
            attempt,
            attempts: self.policy.attempts,
            cause,
            pause,
        })
    }

    /// The policy's stable bound description.
    #[must_use]
    pub fn bound(self) -> String {
        self.policy.bound()
    }
}

/// An injected operation's final failure.
#[derive(Debug, PartialEq, Eq)]
pub struct RunError<E> {
    /// The operation's last error.
    pub error: E,
    /// How many attempts ran.
    pub attempts: u32,
    /// Whether the last error was transient but outside the bound.
    pub exhausted: bool,
    /// The policy's stable bound description.
    pub bound: String,
}

impl<E: std::fmt::Display> std::fmt::Display for RunError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.exhausted {
            write!(f, "{} after {}", self.error, self.bound)
        } else {
            self.error.fmt(f)
        }
    }
}

/// Run an injected idempotent operation under `policy`. `classify` returns `None` for a final
/// answer and `on_retry` records the cause and performs the caller's kind of wait.
pub fn run<T, E>(
    policy: Policy,
    mut operation: impl FnMut(u32) -> Result<T, E>,
    mut classify: impl FnMut(&E) -> Option<Cause>,
    mut on_retry: impl FnMut(Retry),
) -> Result<T, RunError<E>> {
    let mut budget = Budget::new(policy);
    let mut attempt = 1;
    loop {
        match operation(attempt) {
            Ok(value) => return Ok(value),
            Err(error) => {
                let Some(cause) = classify(&error) else {
                    return Err(RunError {
                        error,
                        attempts: attempt,
                        exhausted: false,
                        bound: budget.bound(),
                    });
                };
                match budget.after(attempt, cause) {
                    Decision::Retry(retry) => on_retry(retry),
                    Decision::Exhausted => {
                        return Err(RunError {
                            error,
                            attempts: attempt,
                            exhausted: true,
                            bound: budget.bound(),
                        });
                    }
                }
            }
        }
        attempt = attempt.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
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
}
