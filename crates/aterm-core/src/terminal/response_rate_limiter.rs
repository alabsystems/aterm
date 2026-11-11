// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Token-bucket rate limiter for PTY response sequences.

use aterm_time::Instant;
use std::time::Duration;

/// Default refill rate in bytes per second.
pub(crate) const DEFAULT_REFILL_BYTES_PER_SEC: u64 = 100 * 1024;

/// Default burst capacity in bytes, sized above [`super::MAX_OSC52_QUERY_RESPONSE_BYTES`] in base64.
pub(crate) const DEFAULT_BURST_BYTES: u64 = 96 * 1024;

/// The clock a debit reads: the batch's ([`BatchClock`]) or a test's.
pub(crate) trait TimeSource {
    fn now(&self) -> Instant;
}

/// The batch's injected clock (`process_now`), so a debit refills by the time the program's own output measured and a replay admits what the live parse did.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BatchClock(pub(crate) Instant);

impl TimeSource for BatchClock {
    #[inline]
    fn now(&self) -> Instant {
        self.0
    }
}

impl aterm_policy::limits::TimeSource for BatchClock {
    #[inline]
    fn now(&self) -> Instant {
        self.0
    }
}

/// Token-bucket rate limiter for PTY response bytes.
#[derive(Debug, Clone)]
pub(crate) struct ResponseRateLimiter {
    /// Current token count. Saturates at `capacity_bytes` after refill.
    tokens: f64,
    /// Maximum token count (burst capacity).
    capacity_bytes: u64,
    /// Token refill rate in bytes per second.
    refill_bytes_per_sec: u64,
    /// Last refill instant; `None` until the first `try_consume` seeds it,
    /// so construction reads no clock.
    last_refill: Option<Instant>,
}

impl ResponseRateLimiter {
    /// Creates a rate limiter with default capacity and refill rate.
    #[inline]
    pub(crate) const fn new() -> Self {
        Self::with_limits(DEFAULT_REFILL_BYTES_PER_SEC, DEFAULT_BURST_BYTES)
    }

    /// Creates a rate limiter with custom refill and capacity limits.
    #[inline]
    pub(crate) const fn with_limits(refill_bytes_per_sec: u64, capacity_bytes: u64) -> Self {
        Self {
            #[allow(
                clippy::cast_precision_loss,
                reason = "burst is bytes, precision loss at u64 extremes is not observable"
            )]
            tokens: capacity_bytes as f64,
            capacity_bytes,
            refill_bytes_per_sec,
            last_refill: None,
        }
    }

    /// Deducts `bytes` tokens, returning `false` when the response must be dropped.
    #[inline]
    pub(crate) fn try_consume<T: TimeSource>(&mut self, bytes: usize, clock: &T) -> bool {
        // Larger than capacity can never fit: drop without deducting, so one pathological
        // call does not drain the bucket.
        if bytes as u64 > self.capacity_bytes {
            return false;
        }

        let now = clock.now();
        self.refill(now);

        #[allow(
            clippy::cast_precision_loss,
            reason = "bytes is usize; precision loss only at bytes > 2^53 which is impossible here"
        )]
        let needed = bytes as f64;
        if self.tokens >= needed {
            self.tokens -= needed;
            true
        } else {
            false
        }
    }

    /// Refill tokens based on elapsed time since last refill.
    #[inline]
    fn refill(&mut self, now: Instant) {
        let Some(last) = self.last_refill else {
            self.last_refill = Some(now);
            return;
        };

        let elapsed = now.saturating_duration_since(last);
        if elapsed == Duration::ZERO {
            return;
        }

        #[allow(
            clippy::cast_precision_loss,
            reason = "refill rate is bytes/sec; f64 has >40 bits of precision for any realistic rate"
        )]
        let rate = self.refill_bytes_per_sec as f64;
        let earned = rate * elapsed.as_secs_f64();
        #[allow(
            clippy::cast_precision_loss,
            reason = "capacity is bytes; f64 precision sufficient for any realistic cap"
        )]
        let cap = self.capacity_bytes as f64;
        self.tokens = (self.tokens + earned).min(cap);
        self.last_refill = Some(now);
    }

    /// Snapshot `(tokens, capacity_bytes, refill_bytes_per_sec, last_refill)`.
    #[inline]
    pub(crate) const fn snapshot(&self) -> (f64, u64, u64, Option<Instant>) {
        (
            self.tokens,
            self.capacity_bytes,
            self.refill_bytes_per_sec,
            self.last_refill,
        )
    }

    /// Restore a limiter from a previously captured snapshot, clamping `tokens` into `0.0..=capacity_bytes`.
    #[inline]
    pub(crate) fn from_snapshot(
        tokens: f64,
        capacity_bytes: u64,
        refill_bytes_per_sec: u64,
        last_refill: Option<Instant>,
    ) -> Self {
        #[allow(
            clippy::cast_precision_loss,
            reason = "capacity is bytes; f64 precision sufficient for any realistic cap"
        )]
        let cap = capacity_bytes as f64;
        let mut clamped = if tokens.is_finite() {
            tokens.clamp(0.0, cap)
        } else {
            0.0
        };
        if clamped == 0.0 {
            clamped = 0.0;
        }
        Self {
            tokens: clamped,
            capacity_bytes,
            refill_bytes_per_sec,
            last_refill,
        }
    }
}

impl Default for ResponseRateLimiter {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "response_rate_limiter_tests.rs"]
mod tests;
