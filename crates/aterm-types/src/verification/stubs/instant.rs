// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Counter-based `Instant` replacement for Kani proofs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Global counter for `VerifyInstant::now()`.
///
/// `AtomicU64` (not `static mut`): under Kani's single-threaded execution a
/// relaxed `fetch_add` is exactly the old read-then-increment — same ticks,
/// same order — so proofs see identical values, and no `unsafe` remains for
/// the Trust L0 gate to refute (the old `static mut` access was an unmodeled
/// mutable-static data race, fail-closed). In parallel tests the old form was
/// technically UB; the atomic keeps the per-thread monotonicity the tests
/// assert while making the counter well-defined.
static VERIFY_TIME_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Counter-based `Instant` replacement for Kani proofs.
///
/// Avoids `clock_gettime` FFI that Kani cannot model. Each `now()` call
/// increments a global counter, producing monotonically increasing values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VerifyInstant {
    /// Internal counter value representing "time".
    pub(super) ticks: u64,
}

impl VerifyInstant {
    /// Returns the current "instant" (counter-based, not real time).
    pub fn now() -> Self {
        // Relaxed is enough: only the counter value itself matters (no other
        // memory is published), and `fetch_add` wraps on overflow exactly like
        // the previous `wrapping_add` — behavior-identical to the old
        // `static mut` read-then-increment on every single-threaded execution,
        // with the data race (and the whole `unsafe` block) gone.
        let ticks = VERIFY_TIME_COUNTER.fetch_add(1, Ordering::Relaxed);
        Self { ticks }
    }

    /// Returns the elapsed duration since this instant was created.
    pub fn elapsed(&self) -> Duration {
        Self::now().duration_since(*self)
    }

    /// Returns the duration between `earlier` and `self`.
    ///
    /// Saturates to `Duration::ZERO` when `earlier` is later than `self`,
    /// matching `std::time::Instant::duration_since`. Under the counter's
    /// monotonic-`now()` invariant `self.ticks >= earlier.ticks` always holds,
    /// so `saturating_sub` returns exactly `self.ticks - earlier.ticks`.
    // Skip: web_time::Duration accessors (third-party absent bodies).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn duration_since(&self, earlier: VerifyInstant) -> Duration {
        Duration::from_millis(self.ticks.saturating_sub(earlier.ticks))
    }

    /// Returns `Some(t)` where `t` is the instant `self + duration`, or `None` on overflow.
    // Skip: web_time::Duration accessors (third-party absent bodies).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn checked_add(&self, duration: Duration) -> Option<Self> {
        let millis = duration.as_millis();
        let millis_u64 = u64::try_from(millis).ok()?;
        self.ticks
            .checked_add(millis_u64)
            .map(|ticks| Self { ticks })
    }

    /// Returns `Some(t)` where `t` is the instant `self - duration`, or `None` on underflow.
    // Skip: web_time::Duration accessors (third-party absent bodies).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn checked_sub(&self, duration: Duration) -> Option<Self> {
        let millis = duration.as_millis();
        let millis_u64 = u64::try_from(millis).ok()?;
        self.ticks
            .checked_sub(millis_u64)
            .map(|ticks| Self { ticks })
    }
}

// Mirrors std::time::Instant — Add/Sub panic on overflow per Rust convention.
#[allow(
    clippy::expect_used,
    reason = "mirrors std::time::Instant — Add/Sub panic on overflow per Rust convention"
)]
impl std::ops::Add<Duration> for VerifyInstant {
    type Output = Self;

    // AUDITED CONTRACT PANIC: mirrors `std::time::Instant`'s `Add` contract —
    // panicking on tick overflow is the documented Rust convention for this
    // operator; callers wanting a fallible form use `checked_add`.
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(message_contains = "overflow when adding duration to instant")
    )]
    // Skip: third-party `web_time::Duration` accessors (absent bodies).
    #[cfg_attr(trust_verify, trust::skip)]
    fn add(self, duration: Duration) -> Self {
        self.checked_add(duration)
            .expect("overflow when adding duration to instant")
    }
}

#[allow(
    clippy::expect_used,
    reason = "mirrors std::time::Instant — Add/Sub panic on overflow per Rust convention"
)]
impl std::ops::Sub<Duration> for VerifyInstant {
    type Output = Self;

    // AUDITED CONTRACT PANIC: mirrors `std::time::Instant`'s `Sub` contract —
    // panicking on tick underflow is the documented Rust convention for this
    // operator; callers wanting a fallible form use `checked_sub`.
    #[cfg_attr(
        trust_verify,
        trust::contract_panic(
            message_contains = "overflow when subtracting duration from instant"
        )
    )]
    // Skip: third-party `web_time::Duration` accessors (absent bodies).
    #[cfg_attr(trust_verify, trust::skip)]
    fn sub(self, duration: Duration) -> Self {
        self.checked_sub(duration)
            .expect("overflow when subtracting duration from instant")
    }
}

impl std::ops::Sub<VerifyInstant> for VerifyInstant {
    type Output = Duration;

    fn sub(self, other: VerifyInstant) -> Duration {
        self.duration_since(other)
    }
}

impl Default for VerifyInstant {
    fn default() -> Self {
        Self::now()
    }
}
