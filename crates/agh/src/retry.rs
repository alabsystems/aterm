// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Request retry policy and delay calculation.

use std::time::Duration;

use crate::error::{Error, Kind, Sub};

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
    /// The longest secondary-limit wait taken.
    pub max_secondary_wait: Duration,
}

impl RetryPolicy {
    /// 3 attempts, pauses 2 s then 5 s, a secondary wait of at most 60 s.
    pub const STANDARD: RetryPolicy = RetryPolicy {
        attempts: 3,
        pauses: [Duration::from_secs(2), Duration::from_secs(5)],
        max_secondary_wait: Duration::from_secs(60),
    };

    /// 1 attempt.
    pub const NONE: RetryPolicy = RetryPolicy {
        attempts: 1,
        pauses: [Duration::ZERO, Duration::ZERO],
        max_secondary_wait: Duration::ZERO,
    };
}

/// Returns the pause before attempt `attempt + 1` (`attempt` counts from 1), or `None` to stop.
#[inline]
#[must_use]
pub fn should_retry(
    policy: &RetryPolicy,
    idempotency: Idempotency,
    attempt: u32,
    error: &Error,
    secondary_retried: bool,
) -> Option<Duration> {
    if attempt >= policy.attempts || idempotency == Idempotency::OneShot {
        return None;
    }
    match error.sub {
        Sub::Secondary => {
            let wait = Duration::from_secs(error.retry_after.unwrap_or(ABSENT_RETRY_AFTER));
            (!secondary_retried && wait <= policy.max_secondary_wait).then_some(wait)
        }
        Sub::Primary | Sub::PreRefused => None,
        _ if error.kind == Kind::Server
            || (error.kind == Kind::CouldNotRun && matches!(error.sub, Sub::Wire(_))) =>
        {
            let index = usize::from(attempt >= 2);
            Some(policy.pauses[index])
        }
        _ => None,
    }
}

const ABSENT_RETRY_AFTER: u64 = 60;
