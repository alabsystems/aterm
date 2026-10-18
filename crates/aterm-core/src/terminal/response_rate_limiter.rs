// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Token-bucket rate limiter for PTY response sequences.
//!
//! A malicious PTY peer can spam queries (DSR, DA, DECRQSS, color queries, XTGETTCAP,
//! title reports) to force replies. The 1 MiB [`MAX_RESPONSE_BUFFER_SIZE`] bounds memory
//! but not rate, so [`ResponseRateLimiter`] gates `send_response` and drops the excess, as
//! a full buffer does. The defaults sit far above the few hundred bytes/s of replies a
//! shell startup emits.
//!
//! [`MAX_RESPONSE_BUFFER_SIZE`]: super::MAX_RESPONSE_BUFFER_SIZE

// aterm_time::Instant: std on native, the JS clock on wasm32 (where std's
// Instant::now() panics); the engine's other Instant seams use it too.
use aterm_time::Instant;
use std::time::Duration;

/// Default refill rate in bytes/sec: far above legitimate traffic (shell-startup probes
/// are under 1 KiB in all), while still limiting amplification against the 1 MiB cap.
pub(crate) const DEFAULT_REFILL_BYTES_PER_SEC: u64 = 100 * 1024;

/// Default burst capacity in bytes: absorbs startup bursts (DECRQSS batteries,
/// DA/DA2/XTVERSION, OSC palette probes) and carries the largest response. Sized on the
/// wire: an OSC 52 query answers in base64, about 1.34x the
/// [`MAX_OSC52_QUERY_RESPONSE_BYTES`] the handler admits, and `try_consume` refuses
/// anything above capacity.
///
/// [`MAX_OSC52_QUERY_RESPONSE_BYTES`]: super::MAX_OSC52_QUERY_RESPONSE_BYTES
pub(crate) const DEFAULT_BURST_BYTES: u64 = 96 * 1024;

/// Abstraction over [`Instant::now`] so tests can drive the clock forward
/// deterministically without sleeping.
pub(crate) trait TimeSource {
    /// Return the current instant.
    fn now(&self) -> Instant;
}

/// Production time source — delegates to [`Instant::now`].
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SystemTime;

impl TimeSource for SystemTime {
    #[inline]
    #[expect(
        clippy::disallowed_methods,
        reason = "response-rate gating only: throttles when reply bytes go out, never feeds grid, cursor or mode state"
    )]
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Token-bucket rate limiter for PTY response bytes: tokens refill at
/// `refill_bytes_per_sec` up to `capacity_bytes`, and a response that finds too few is
/// dropped silently. One larger than the capacity is always dropped, so the capacity is
/// sized on the largest encoded response ([`DEFAULT_BURST_BYTES`]).
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
    /// Create a rate limiter with the default capacity and refill rate.
    pub(crate) fn new() -> Self {
        Self::with_limits(DEFAULT_REFILL_BYTES_PER_SEC, DEFAULT_BURST_BYTES)
    }

    /// A rate limiter with custom limits. `refill_bytes_per_sec = 0` never replenishes the
    /// initial capacity.
    pub(crate) fn with_limits(refill_bytes_per_sec: u64, capacity_bytes: u64) -> Self {
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

    /// Deduct `bytes` tokens: `true` if the response may go, `false` if it is dropped.
    /// `f64` tokens let sub-millisecond refill intervals accumulate (a burst of tiny DSR
    /// replies back to back).
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
    fn refill(&mut self, now: Instant) {
        let Some(last) = self.last_refill else {
            // First call: seed the clock; the bucket already starts full.
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
}

impl Default for ResponseRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Advances only when `advance()`d.
    struct FakeClock {
        now: Cell<Instant>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                now: Cell::new(Instant::now()),
            }
        }

        fn advance(&self, by: Duration) {
            self.now.set(self.now.get() + by);
        }
    }

    impl TimeSource for FakeClock {
        fn now(&self) -> Instant {
            self.now.get()
        }
    }

    #[test]
    fn test_response_rate_above_limit_excess_dropped() {
        // No time passes: the 200-byte burst admits five 40-byte responses.
        let mut limiter = ResponseRateLimiter::with_limits(100, 200);
        let clock = FakeClock::new();

        let mut allowed = 0;
        let mut denied = 0;
        for _ in 0..10 {
            if limiter.try_consume(40, &clock) {
                allowed += 1;
            } else {
                denied += 1;
            }
        }

        assert_eq!(
            allowed, 5,
            "burst capacity should permit exactly 5 x 40-byte responses"
        );
        assert_eq!(denied, 5, "excess over burst must be silently dropped");
    }

    #[test]
    fn test_response_rate_replenishes_after_cooldown() {
        let mut limiter = ResponseRateLimiter::with_limits(1000, 100);
        let clock = FakeClock::new();

        assert!(limiter.try_consume(100, &clock));
        assert!(!limiter.try_consume(1, &clock));

        // 50 ms at 1000 bytes/sec earns 50 tokens back.
        clock.advance(Duration::from_millis(50));
        assert!(
            limiter.try_consume(50, &clock),
            "tokens should replenish after cooldown"
        );
        assert!(!limiter.try_consume(1, &clock));
    }

    #[test]
    fn test_response_burst_capacity_caps_at_configured_max() {
        let mut limiter = ResponseRateLimiter::with_limits(10_000, 100);
        let clock = FakeClock::new();

        assert!(limiter.try_consume(100, &clock));

        // Ten idle seconds would earn 100_000 tokens uncapped.
        clock.advance(Duration::from_secs(10));
        assert!(
            limiter.try_consume(100, &clock),
            "full burst should be available after idle"
        );
        assert!(
            !limiter.try_consume(1, &clock),
            "bucket must cap at configured capacity"
        );
    }

    #[test]
    fn test_response_oversized_always_dropped() {
        let mut limiter = ResponseRateLimiter::with_limits(1000, 100);
        let clock = FakeClock::new();

        assert!(
            !limiter.try_consume(101, &clock),
            "response larger than capacity must be dropped"
        );
        // Nor is the bucket drained by it.
        assert!(limiter.try_consume(100, &clock));
    }
}
