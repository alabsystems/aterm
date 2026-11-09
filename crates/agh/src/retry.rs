// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Request retry classification and the shared anonymous-HTTP retry budget.

use std::time::Duration;

use aterm_types::http_retry::{
    Budget, Cause, Decision as SharedDecision, Policy, Retry as SharedRetry,
};

use crate::error::{Error, Kind, Sub};
use crate::wire::WireKind;

/// Whether a request may be sent again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Idempotency {
    /// A read: always safe to repeat.
    Read,
    /// A write that converges: repeating it lands in the same state.
    Converging,
    /// A write that must be sent once.
    OneShot,
}

/// How a read retries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// The most attempts, the first included.
    pub attempts: u32,
    /// The pauses before the second and the later attempts.
    pub pauses: [Duration; 2],
    /// The longest `Retry-After` wait taken.
    pub max_secondary_wait: Duration,
}

impl RetryPolicy {
    /// The existing authenticated/read-write policy: 3 attempts, pauses 2 s then 5 s, and a
    /// `Retry-After` wait capped at 60 s.
    pub const STANDARD: RetryPolicy = RetryPolicy {
        attempts: 3,
        pauses: [Duration::from_secs(2), Duration::from_secs(5)],
        max_secondary_wait: Duration::from_secs(60),
    };

    /// Credential-free HTTPS reads: the shared 19-attempt, 10 s / 200 s pause policy.
    pub const ANONYMOUS_HTTP: RetryPolicy = RetryPolicy {
        attempts: 19,
        pauses: [Duration::from_secs(10), Duration::from_secs(10)],
        max_secondary_wait: Duration::from_secs(200),
    };

    /// 1 attempt.
    pub const NONE: RetryPolicy = RetryPolicy {
        attempts: 1,
        pauses: [Duration::ZERO, Duration::ZERO],
        max_secondary_wait: Duration::ZERO,
    };

    /// A fresh shared retry budget for one operation.
    #[must_use]
    pub fn budget(self) -> RetryBudget {
        RetryBudget {
            inner: Budget::new(self.shared()),
            policy: self,
        }
    }

    /// A budget after an earlier phase of the same composite read already spent `paused`.
    #[must_use]
    pub fn budget_with_paused(self, paused: Duration) -> Option<RetryBudget> {
        Some(RetryBudget {
            inner: Budget::with_paused(self.shared(), paused)?,
            policy: self,
        })
    }

    /// The shared policy's stable exhaustion bound.
    #[must_use]
    pub fn bound(self) -> String {
        self.shared().bound()
    }

    /// Map the ordinary production policy to the fleet-wide anonymous-HTTPS policy. An explicit
    /// `NONE` or custom policy remains exact, which keeps injected transports deterministic.
    #[must_use]
    pub fn for_anonymous_http(self) -> RetryPolicy {
        if self == Self::STANDARD {
            Self::ANONYMOUS_HTTP
        } else {
            self
        }
    }

    fn shared(self) -> Policy {
        if self == Self::ANONYMOUS_HTTP {
            return Policy::STANDARD;
        }
        if self == Self::NONE {
            return Policy::NONE;
        }

        // Every retry may carry Retry-After. Bound each slot by the larger of its ordinary
        // pause and the admitted header; this is a pause/progress bound, never a request-
        // latency budget.
        let retries = self.attempts.saturating_sub(1);
        let tail = self.pauses[1]
            .max(self.max_secondary_wait)
            .checked_mul(retries.saturating_sub(1))
            .unwrap_or(Duration::MAX);
        let head = self.pauses[0].max(self.max_secondary_wait);
        let max_total_pause = if retries == 0 {
            Duration::ZERO
        } else {
            head.saturating_add(tail)
        };
        Policy::new(
            self.attempts,
            self.pauses,
            self.max_secondary_wait,
            max_total_pause,
        )
    }
}

/// The shared retry state for one operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryBudget {
    inner: Budget,
    policy: RetryPolicy,
}

impl RetryBudget {
    /// Apply this operation's shared budget to a proven transient cause.
    #[must_use]
    pub fn after(&mut self, attempt: u32, cause: Cause) -> RetryDecision {
        if self.policy == RetryPolicy::NONE {
            return RetryDecision::Final;
        }
        match self.inner.after(attempt, cause) {
            SharedDecision::Retry(retry) => RetryDecision::Retry(retry),
            SharedDecision::Exhausted => RetryDecision::Exhausted {
                bound: self.inner.bound(),
            },
        }
    }
}

/// What follows one failed attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetryDecision {
    /// Repeat after this logged, bounded wait.
    Retry(SharedRetry),
    /// The failure is transient, but no further attempt is within the shared bound.
    Exhausted {
        /// The bound suitable for the final refusal.
        bound: String,
    },
    /// This class is not transient, or the request is one-shot.
    Final,
}

/// The exact transient class of `error`.
///
/// An answered 4xx takes precedence over a wire sub-kind. In particular, neither a 404 nor a
/// 429 without a valid delta-seconds or HTTP-date `Retry-After` can be turned into a retry by a
/// receive error.
#[must_use]
pub fn transient_cause(error: &Error) -> Option<Cause> {
    if let Some(http) = &error.http {
        if let Some(cause) = Cause::from_http(http.status, error.retry_after) {
            return Some(cause);
        }
        if (400..=499).contains(&http.status) {
            return None;
        }
    }

    if error.kind == Kind::Server {
        return Some(Cause::Server(
            error.http.as_ref().map_or(500, |http| http.status),
        ));
    }

    match error.sub {
        Sub::Wire(WireKind::Timeout | WireKind::Idle) => Some(Cause::Timeout),
        Sub::Wire(WireKind::Reset) => Some(Cause::Reset),
        Sub::Wire(WireKind::Truncated) => Some(Cause::Truncated),
        _ => None,
    }
}

/// Apply the shared retry policy to `error` from `attempt` (numbered from one).
#[must_use]
pub fn should_retry(
    budget: &mut RetryBudget,
    idempotency: Idempotency,
    attempt: u32,
    error: &Error,
) -> RetryDecision {
    if idempotency == Idempotency::OneShot {
        return RetryDecision::Final;
    }
    let Some(cause) = transient_cause(error) else {
        return RetryDecision::Final;
    };
    budget.after(attempt, cause)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_custom_policy_accounts_for_retry_after_in_every_slot() {
        let policy = RetryPolicy {
            attempts: 3,
            pauses: [Duration::from_secs(1), Duration::from_secs(2)],
            max_secondary_wait: Duration::from_secs(5),
        };
        assert_eq!(policy.bound(), "3 attempts / 10s total retry-pause bound");
        let cause = Cause::RateLimited {
            retry_after: Duration::from_secs(5),
        };
        let mut budget = policy.budget();
        assert!(matches!(
            budget.after(1, cause),
            RetryDecision::Retry(retry) if retry.pause == Duration::from_secs(5)
        ));
        assert!(matches!(
            budget.after(2, cause),
            RetryDecision::Retry(retry) if retry.pause == Duration::from_secs(5)
        ));
        assert!(matches!(
            budget.after(3, cause),
            RetryDecision::Exhausted { .. }
        ));
    }
}
