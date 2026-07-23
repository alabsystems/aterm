// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Token-bucket rate limiter for PTY response sequences (Part of #7874).
//!
//! # Threat model
//!
//! A malicious PTY peer can spam query sequences (DSR, DA, DECRQSS, color
//! queries, XTGETTCAP, title reports, etc.) to force the terminal engine to
//! generate response bytes. The existing [`MAX_RESPONSE_BUFFER_SIZE`] cap
//! (1 MiB, see `terminal/mod.rs`) prevents unbounded memory growth but does
//! nothing about the *rate* of response generation: a host draining the
//! buffer in a tight loop still pays CPU and wire-bandwidth cost for every
//! response the engine is forced to emit.
//!
//! [`ResponseRateLimiter`] gates `send_response` with a token bucket so the
//! engine can silently drop excess responses once a configurable rate is
//! exceeded. This matches the existing buffer-full behavior — sequences
//! that cannot be delivered are simply not emitted.
//!
//! # Tuning
//!
//! Defaults:
//! - Refill rate: [`DEFAULT_REFILL_BYTES_PER_SEC`] = 100 KiB/s
//! - Burst capacity: [`DEFAULT_BURST_BYTES`] = 64 KiB
//!
//! Rationale: normal terminals emit at most a few hundred bytes/s of
//! responses during shell startup (DA/DA2/XTVERSION/DECRQSS probes).
//! The defaults are ~500x above that ceiling, so they never trip for
//! legitimate traffic while still capping abusive peers to a tiny
//! fraction of the 1 MiB buffer's worst case rate.
//!
//! Hosts can tune via [`Terminal::set_response_rate_limit`][set].
//!
//! [`MAX_RESPONSE_BUFFER_SIZE`]: super::MAX_RESPONSE_BUFFER_SIZE
//! [set]: super::state::Terminal::set_response_rate_limit

// web_time::Instant: std::time on native (byte-identical), JS clock on wasm32 where
// Instant::now() panics. Must match the engine's other Instant seams (bell, transient).
use std::time::Duration;
use web_time::Instant;

/// Default refill rate in bytes/sec. 100 KiB/s is far above the peak
/// legitimate response traffic (shell-startup probes are <1 KiB total)
/// while still reducing amplification relative to the 1 MiB buffer cap.
pub(crate) const DEFAULT_REFILL_BYTES_PER_SEC: u64 = 100 * 1024;

/// Default burst capacity in bytes. 64 KiB absorbs legitimate startup
/// bursts (DECRQSS batteries, multiple DA/DA2/XTVERSION queries, OSC
/// palette probes) with room to spare.
pub(crate) const DEFAULT_BURST_BYTES: u64 = 64 * 1024;

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
    fn now(&self) -> Instant {
        Instant::now() // CLOCK-EXEMPT: response-RATE gating only; throttles when reply BYTES are emitted, never feeds hydrated grid/cursor/mode state, so Terminal::process() stays deterministic.
    }
}

/// Token-bucket rate limiter for PTY response bytes.
///
/// Each call to [`try_consume`][Self::try_consume] attempts to deduct
/// `n` tokens; on success the response is allowed, otherwise it is
/// silently dropped. Tokens refill at `refill_bytes_per_sec` and the
/// bucket is capped at `capacity_bytes`.
///
/// Responses larger than `capacity_bytes` are always dropped — a single
/// legitimate response never exceeds the [`MAX_OSC52_QUERY_RESPONSE_BYTES`]
/// cap (64 KiB) by construction, so with the default burst capacity this
/// is purely defensive.
///
/// [`MAX_OSC52_QUERY_RESPONSE_BYTES`]: super::MAX_OSC52_QUERY_RESPONSE_BYTES
#[derive(Debug, Clone)]
pub(crate) struct ResponseRateLimiter {
    /// Current token count. Saturates at `capacity_bytes` after refill.
    tokens: f64,
    /// Maximum token count (burst capacity).
    capacity_bytes: u64,
    /// Token refill rate in bytes per second.
    refill_bytes_per_sec: u64,
    /// Last refill instant. `None` means "uninitialized — seed on next call"
    /// so the first `try_consume` after construction always succeeds at
    /// full capacity without a time lookup during construction.
    last_refill: Option<Instant>,
}

impl ResponseRateLimiter {
    /// Create a rate limiter with the default capacity and refill rate.
    pub(crate) fn new() -> Self {
        Self::with_limits(DEFAULT_REFILL_BYTES_PER_SEC, DEFAULT_BURST_BYTES)
    }

    /// Create a rate limiter with custom limits.
    ///
    /// `capacity_bytes = 0` disables the limiter (every response is dropped —
    /// useful as a kill switch). `refill_bytes_per_sec = 0` freezes tokens
    /// at the initial capacity (no replenishment).
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

    /// Reconfigure the limiter in place, preserving current token count
    /// up to the new capacity.
    pub(crate) fn reconfigure(&mut self, refill_bytes_per_sec: u64, capacity_bytes: u64) {
        self.refill_bytes_per_sec = refill_bytes_per_sec;
        self.capacity_bytes = capacity_bytes;
        #[allow(
            clippy::cast_precision_loss,
            reason = "capacity is bytes, precision loss at u64 extremes is not observable"
        )]
        let cap = capacity_bytes as f64;
        if self.tokens > cap {
            self.tokens = cap;
        }
    }

    /// Attempt to deduct `bytes` tokens using the given time source.
    ///
    /// Returns `true` if the response is permitted, `false` if it must be
    /// dropped. Uses `f64` internally so sub-millisecond refill intervals
    /// accumulate correctly (important when a shell emits a burst of tiny
    /// DSR responses back-to-back).
    pub(crate) fn try_consume<T: TimeSource>(&mut self, bytes: usize, clock: &T) -> bool {
        // A limiter with zero capacity is a hard block — no refill can help.
        if self.capacity_bytes == 0 {
            return false;
        }
        // Responses larger than capacity can never fit; drop without
        // deducting so a single pathological call doesn't drain the bucket.
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
            // First call: seed the clock but don't grant extra tokens —
            // the bucket already starts at full capacity.
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

    /// Current token balance (rounded down to bytes). Intended for tests
    /// and diagnostic assertions only.
    #[cfg(test)]
    pub(crate) fn tokens(&self) -> u64 {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "tokens are clamped to [0, capacity] which fits in u64"
        )]
        let t = self.tokens as u64;
        t
    }

    /// Current burst capacity.
    #[cfg(test)]
    pub(crate) fn capacity(&self) -> u64 {
        self.capacity_bytes
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

    /// Test clock: advances only when explicitly `advance()`d.
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
    fn test_response_rate_below_limit_all_delivered() {
        // 1000 bytes/sec, 500-byte burst. Ten 40-byte responses spaced at
        // 200ms each consume 400 bytes total and earn 200 bytes back before
        // the first deduction catches up — well below the limit.
        let mut limiter = ResponseRateLimiter::with_limits(1000, 500);
        let clock = FakeClock::new();

        for _ in 0..10 {
            assert!(
                limiter.try_consume(40, &clock),
                "normal-rate response should be permitted"
            );
            clock.advance(Duration::from_millis(200));
        }
    }

    #[test]
    fn test_response_rate_above_limit_excess_dropped() {
        // 100 bytes/sec refill, 200-byte burst. Fire 10 responses of 40
        // bytes with no time advancing: first 5 (= 200 bytes) succeed,
        // remaining 5 must be dropped.
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
        // Drain the bucket entirely, then advance time and confirm tokens
        // return at the configured refill rate.
        let mut limiter = ResponseRateLimiter::with_limits(1000, 100);
        let clock = FakeClock::new();

        // Drain all 100 tokens in one shot.
        assert!(limiter.try_consume(100, &clock));
        // Now dry — even a 1-byte request should fail.
        assert!(!limiter.try_consume(1, &clock));

        // Advance 50ms: at 1000 bytes/sec that earns 50 tokens back.
        clock.advance(Duration::from_millis(50));
        assert!(
            limiter.try_consume(50, &clock),
            "tokens should replenish after cooldown"
        );
        // One more byte beyond what we earned should fail.
        assert!(!limiter.try_consume(1, &clock));
    }

    #[test]
    fn test_response_burst_capacity_caps_at_configured_max() {
        // Refill at 10_000/sec into a tiny 100-byte bucket, then idle for
        // a long time: the bucket must cap at 100, not accumulate forever.
        let mut limiter = ResponseRateLimiter::with_limits(10_000, 100);
        let clock = FakeClock::new();

        // Drain the initial 100 tokens.
        assert!(limiter.try_consume(100, &clock));

        // Idle for 10 seconds: would earn 100_000 tokens uncapped, but
        // cap keeps us at 100.
        clock.advance(Duration::from_secs(10));
        assert!(
            limiter.try_consume(100, &clock),
            "full burst should be available after idle"
        );
        // Can't exceed capacity — next byte denied.
        assert!(
            !limiter.try_consume(1, &clock),
            "bucket must cap at configured capacity"
        );
    }

    #[test]
    fn test_response_oversized_always_dropped() {
        // A single response larger than capacity cannot fit.
        let mut limiter = ResponseRateLimiter::with_limits(1000, 100);
        let clock = FakeClock::new();

        assert!(
            !limiter.try_consume(101, &clock),
            "response larger than capacity must be dropped"
        );
        // But the bucket should not have been drained — subsequent
        // well-sized calls still succeed.
        assert!(limiter.try_consume(100, &clock));
    }

    #[test]
    fn test_response_zero_capacity_blocks_everything() {
        let mut limiter = ResponseRateLimiter::with_limits(10_000, 0);
        let clock = FakeClock::new();
        assert!(!limiter.try_consume(1, &clock));
        assert!(!limiter.try_consume(0, &clock));
    }

    #[test]
    fn test_response_reconfigure_preserves_tokens_under_new_cap() {
        let mut limiter = ResponseRateLimiter::with_limits(1000, 500);
        let clock = FakeClock::new();
        // Spend 200 tokens → 300 remaining.
        assert!(limiter.try_consume(200, &clock));
        assert_eq!(limiter.tokens(), 300);

        // Shrink capacity below current balance: tokens must be clamped.
        limiter.reconfigure(1000, 100);
        assert_eq!(limiter.capacity(), 100);
        assert!(limiter.tokens() <= 100);
    }

    #[test]
    fn test_response_defaults_are_permissive() {
        // Sanity-check that the production default permits a realistic
        // startup burst (say, 64 DSR/DA responses of 16 bytes each = 1 KiB).
        let mut limiter = ResponseRateLimiter::new();
        let clock = FakeClock::new();
        for _ in 0..64 {
            assert!(
                limiter.try_consume(16, &clock),
                "defaults must permit normal startup-burst traffic"
            );
        }
    }
}
