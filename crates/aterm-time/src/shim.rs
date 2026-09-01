// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `wasm32` clock: a monotonic [`Instant`] over `performance.now()` and a
//! wall-clock [`SystemTime`] over `Date.now()`.
//!
//! # This module is compiled on native too — under `cfg(test)`
//!
//! `wasm32-unknown-unknown` needs its own std and the Trust toolchain ships
//! none, so `cargo xtask gate web` cross-compiles this crate on upstream
//! stable. That proves it still COMPILES; it executes nothing. A
//! browser-only module would therefore be code that no test on the machine
//! writing it can reach — which is exactly how a clock ships broken.
//!
//! So everything here except the two JS bindings and the `now()` calls that use
//! them is target-independent, and the module is compiled into the NATIVE test
//! build as well. The `tests` at the bottom drive the same arithmetic the
//! browser will run, on a box with no wasm target installed. What remains
//! genuinely unverifiable locally is two `extern` declarations and the two
//! one-line `now()` bodies that call them.

use core::ops::{Add, AddAssign, Sub, SubAssign};
use core::time::Duration;

/// The browser's two clock functions.
///
/// Declared as bare bindings rather than reached through `js-sys`/`web-sys`,
/// because two function imports do not need a typed DOM binding layer — and
/// because `js_namespace` resolves the global at CALL time, so one build works
/// in a window, a worker and a worklet alike.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod js {
    use wasm_bindgen::prelude::wasm_bindgen;

    // `unsafe extern` is the edition-2024 spelling of a foreign block. It says
    // nothing about how these are CALLED: `#[wasm_bindgen]` consumes the block
    // and emits its own safe wrappers, because neither function is declared
    // `unsafe fn`.
    #[wasm_bindgen]
    unsafe extern "C" {
        /// `performance.now()` — milliseconds since the document's time
        /// origin, monotonic, sub-millisecond where the embedder allows it.
        #[wasm_bindgen(js_namespace = performance, js_name = now)]
        pub fn performance_now() -> f64;

        /// `Date.now()` — integer milliseconds since the Unix epoch, wall
        /// clock, and free to jump when the host's clock is corrected.
        #[wasm_bindgen(js_namespace = Date, js_name = now)]
        pub fn date_now() -> f64;
    }
}

/// Convert JS milliseconds to a [`Duration`], TOTALLY.
///
/// `Duration::from_secs_f64` panics on a negative or non-finite input, and a
/// browser clock is not obliged to be either: cross-origin isolation, Spectre
/// clamping and tab suspension all perturb it, and `NaN` is a legal `f64` for a
/// binding to return. Everything unusable floors to zero, which is the only
/// answer that keeps a terminal drawing.
///
/// Whole milliseconds are split from the fraction BEFORE scaling, so precision
/// does not decay as the time origin ages: after an hour `ms` is ~3.6e6, and a
/// naive `ms * 1e6` nanosecond conversion would have spent the mantissa on the
/// integer part.
fn duration_from_millis_f64(ms: f64) -> Duration {
    if !ms.is_finite() || ms <= 0.0 {
        return Duration::ZERO;
    }
    let whole = ms.trunc();
    let frac = ms - whole;
    // `whole` is finite and positive here; beyond `u64::MAX` milliseconds
    // (585 million years) it saturates rather than wrapping.
    let millis = if whole >= u64::MAX as f64 {
        u64::MAX
    } else {
        // Truncating on purpose: `whole` is already integral.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "guarded above: finite, > 0, and below u64::MAX"
        )]
        {
            whole as u64
        }
    };
    // `frac` is in `[0, 1)`, so the nanosecond part is in `[0, 1e6)` — always
    // inside `Duration`'s subsec range, so the sum below can never carry.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "frac is in [0, 1), so the product is in [0, 1e6)"
    )]
    let nanos = (frac * 1_000_000.0) as u32;
    Duration::from_millis(millis).saturating_add(Duration::from_nanos(u64::from(nanos)))
}

/// A measurement of the browser's MONOTONIC clock — the `wasm32` stand-in for
/// `std::time::Instant`, with the same contract.
///
/// Opaque by construction: the only meaning an `Instant` has is its difference
/// from another one.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Instant(Duration);

impl Instant {
    /// Read the monotonic clock.
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    #[must_use]
    pub fn now() -> Self {
        Self::from_js_millis(js::performance_now())
    }

    /// Build from a raw JS millisecond reading. The single seam the native
    /// tests drive, so the conversion and every operation below are exercised
    /// without a browser.
    fn from_js_millis(ms: f64) -> Self {
        Self(duration_from_millis_f64(ms))
    }

    /// Time elapsed from `earlier` to `self`, SATURATING to zero when `earlier`
    /// is the later of the two — matching `std` since 1.60.
    #[must_use]
    pub fn duration_since(&self, earlier: Self) -> Duration {
        self.saturating_duration_since(earlier)
    }

    /// Time elapsed from `earlier` to `self`, or `None` if `earlier` is later.
    #[must_use]
    pub fn checked_duration_since(&self, earlier: Self) -> Option<Duration> {
        self.0.checked_sub(earlier.0)
    }

    /// Time elapsed from `earlier` to `self`, or zero if `earlier` is later.
    #[must_use]
    pub fn saturating_duration_since(&self, earlier: Self) -> Duration {
        self.0.saturating_sub(earlier.0)
    }

    /// Time elapsed since this instant was taken.
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        Self::now().saturating_duration_since(*self)
    }

    /// `self + duration`, or `None` on overflow.
    #[must_use]
    pub fn checked_add(&self, duration: Duration) -> Option<Self> {
        self.0.checked_add(duration).map(Self)
    }

    /// `self - duration`, or `None` if it would precede the time origin.
    #[must_use]
    pub fn checked_sub(&self, duration: Duration) -> Option<Self> {
        self.0.checked_sub(duration).map(Self)
    }
}

impl Add<Duration> for Instant {
    type Output = Self;
    /// # Panics
    /// On overflow, exactly like `std::time::Instant`.
    fn add(self, rhs: Duration) -> Self {
        self.checked_add(rhs)
            .expect("overflow when adding duration to instant")
    }
}

impl AddAssign<Duration> for Instant {
    fn add_assign(&mut self, rhs: Duration) {
        *self = *self + rhs;
    }
}

impl Sub<Duration> for Instant {
    type Output = Self;
    /// # Panics
    /// On underflow, exactly like `std::time::Instant`.
    fn sub(self, rhs: Duration) -> Self {
        self.checked_sub(rhs)
            .expect("overflow when subtracting duration from instant")
    }
}

impl SubAssign<Duration> for Instant {
    fn sub_assign(&mut self, rhs: Duration) {
        *self = *self - rhs;
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;
    /// Saturating, matching `std`'s `Instant - Instant`.
    fn sub(self, rhs: Self) -> Duration {
        self.saturating_duration_since(rhs)
    }
}

/// The anchor of [`SystemTime`]: 1970-01-01 00:00:00 UTC.
pub const UNIX_EPOCH: SystemTime = SystemTime(Duration::ZERO);

/// A measurement of the browser's WALL clock — the `wasm32` stand-in for
/// `std::time::SystemTime`.
///
/// Not monotonic: the value can move backwards when the host's clock is
/// corrected, which is exactly why
/// [`duration_since`](SystemTime::duration_since) returns a `Result`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SystemTime(Duration);

impl SystemTime {
    /// The anchor, as an associated constant — the `std` spelling.
    pub const UNIX_EPOCH: Self = UNIX_EPOCH;

    /// Read the wall clock.
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    #[must_use]
    pub fn now() -> Self {
        Self::from_js_millis(js::date_now())
    }

    /// Build from a raw JS millisecond reading — the native tests' seam.
    fn from_js_millis(ms: f64) -> Self {
        Self(duration_from_millis_f64(ms))
    }

    /// Time elapsed from `earlier` to `self`.
    ///
    /// # Errors
    /// When `earlier` is LATER than `self` — the clock moved backwards, or the
    /// caller passed the arguments the other way round. The error carries the
    /// magnitude of the reversal, as `std` does.
    pub fn duration_since(&self, earlier: Self) -> Result<Duration, SystemTimeError> {
        self.0
            .checked_sub(earlier.0)
            .ok_or_else(|| SystemTimeError(earlier.0.saturating_sub(self.0)))
    }

    /// Time elapsed since this measurement.
    ///
    /// # Errors
    /// When the wall clock has moved backwards since `self` was taken.
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    pub fn elapsed(&self) -> Result<Duration, SystemTimeError> {
        Self::now().duration_since(*self)
    }

    /// `self + duration`, or `None` on overflow.
    #[must_use]
    pub fn checked_add(&self, duration: Duration) -> Option<Self> {
        self.0.checked_add(duration).map(Self)
    }

    /// `self - duration`, or `None` if it would precede the epoch.
    #[must_use]
    pub fn checked_sub(&self, duration: Duration) -> Option<Self> {
        self.0.checked_sub(duration).map(Self)
    }
}

impl Add<Duration> for SystemTime {
    type Output = Self;
    /// # Panics
    /// On overflow, exactly like `std::time::SystemTime`.
    fn add(self, rhs: Duration) -> Self {
        self.checked_add(rhs)
            .expect("overflow when adding duration to system time")
    }
}

impl AddAssign<Duration> for SystemTime {
    fn add_assign(&mut self, rhs: Duration) {
        *self = *self + rhs;
    }
}

impl Sub<Duration> for SystemTime {
    type Output = Self;
    /// # Panics
    /// On underflow, exactly like `std::time::SystemTime`.
    fn sub(self, rhs: Duration) -> Self {
        self.checked_sub(rhs)
            .expect("overflow when subtracting duration from system time")
    }
}

impl SubAssign<Duration> for SystemTime {
    fn sub_assign(&mut self, rhs: Duration) {
        *self = *self - rhs;
    }
}

/// The error [`SystemTime::duration_since`] returns when the wall clock ran
/// backwards. Carries how far back, so a caller can log the magnitude.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SystemTimeError(Duration);

impl SystemTimeError {
    /// How far the second time is AHEAD of the first.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.0
    }
}

impl core::fmt::Display for SystemTimeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "second time provided was later than self")
    }
}

impl std::error::Error for SystemTimeError {}

#[cfg(test)]
mod tests {
    use super::{
        Duration, Instant, SystemTime, SystemTimeError, UNIX_EPOCH, duration_from_millis_f64,
    };

    /// The clock conversion is TOTAL: every `f64` a browser binding could
    /// return produces a `Duration` rather than a panic.
    #[test]
    fn millisecond_conversion_is_total() {
        for ms in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -0.0,
            -1.0,
            -1e300,
            0.0,
            f64::MIN_POSITIVE,
            1e300,
            f64::MAX,
        ] {
            let _ = duration_from_millis_f64(ms);
        }
        assert_eq!(duration_from_millis_f64(f64::NAN), Duration::ZERO);
        assert_eq!(duration_from_millis_f64(f64::INFINITY), Duration::ZERO);
        assert_eq!(duration_from_millis_f64(-1.0), Duration::ZERO);
        assert_eq!(duration_from_millis_f64(0.0), Duration::ZERO);
    }

    /// Whole and fractional milliseconds both survive the conversion.
    #[test]
    fn millisecond_conversion_keeps_the_fraction() {
        assert_eq!(duration_from_millis_f64(1.0), Duration::from_millis(1));
        assert_eq!(
            duration_from_millis_f64(1500.0),
            Duration::from_millis(1500)
        );
        assert_eq!(duration_from_millis_f64(0.5), Duration::from_nanos(500_000));
        assert_eq!(
            duration_from_millis_f64(1.25),
            Duration::from_nanos(1_250_000)
        );
    }

    /// Sub-microsecond resolution survives an AGED time origin — the reason
    /// the whole part is split off before scaling. A tab open for an hour must
    /// still be able to distinguish two frames.
    #[test]
    fn resolution_survives_an_aged_time_origin() {
        let hour_ms = 3_600_000.0_f64;
        let a = duration_from_millis_f64(hour_ms);
        let b = duration_from_millis_f64(hour_ms + 0.1);
        assert!(
            b > a && b - a >= Duration::from_nanos(90_000),
            "0.1ms apart after an hour resolved as {:?}",
            b.saturating_sub(a)
        );
    }

    /// A clock reading beyond `u64::MAX` milliseconds saturates instead of
    /// wrapping into a time in the past.
    #[test]
    fn absurd_readings_saturate() {
        let huge = duration_from_millis_f64(f64::MAX);
        assert_eq!(huge, Duration::from_millis(u64::MAX));
    }

    /// `Instant` differences: forward is the gap, backward is zero (with the
    /// `checked_` form reporting `None` instead).
    #[test]
    fn instant_differences_saturate_backwards() {
        let a = Instant::from_js_millis(1000.0);
        let b = Instant::from_js_millis(1500.0);
        assert_eq!(b.duration_since(a), Duration::from_millis(500));
        assert_eq!(b.saturating_duration_since(a), Duration::from_millis(500));
        assert_eq!(b - a, Duration::from_millis(500));
        assert_eq!(a.duration_since(b), Duration::ZERO);
        assert_eq!(a.saturating_duration_since(b), Duration::ZERO);
        assert_eq!(a.checked_duration_since(b), None);
        assert_eq!(
            b.checked_duration_since(a),
            Some(Duration::from_millis(500))
        );
        assert!(b > a);
    }

    /// `Instant` duration arithmetic round-trips, and the checked forms report
    /// the boundary rather than wrapping through it.
    #[test]
    fn instant_arithmetic_round_trips_and_checks() {
        let a = Instant::from_js_millis(1000.0);
        let mut b = a;
        b += Duration::from_secs(1);
        assert_eq!(b, a + Duration::from_secs(1));
        b -= Duration::from_secs(1);
        assert_eq!(b, a);
        assert_eq!(
            a - Duration::from_millis(1000),
            Instant::from_js_millis(0.0)
        );
        assert_eq!(a.checked_sub(Duration::from_secs(60)), None);
        assert!(a.checked_add(Duration::from_secs(60)).is_some());
        assert_eq!(a.checked_add(Duration::MAX), None);
    }

    /// `SystemTime` is anchored at the epoch, and a backwards step is an ERROR
    /// carrying the magnitude — never a panic and never a wrap.
    #[test]
    fn system_time_reports_backwards_steps() {
        let t0 = SystemTime::from_js_millis(1_700_000_000_000.0);
        let t1 = SystemTime::from_js_millis(1_700_000_001_000.0);
        assert_eq!(
            t1.duration_since(t0).expect("forward"),
            Duration::from_secs(1)
        );
        assert_eq!(
            t0.duration_since(UNIX_EPOCH).expect("after the epoch"),
            Duration::from_millis(1_700_000_000_000)
        );
        assert_eq!(SystemTime::UNIX_EPOCH, UNIX_EPOCH);
        let err: SystemTimeError = t0.duration_since(t1).expect_err("backwards");
        assert_eq!(err.duration(), Duration::from_secs(1));
        assert_eq!(err.to_string(), "second time provided was later than self");
        // The trait object form the `?` operator produces must still work.
        let boxed: Box<dyn std::error::Error> = Box::new(err);
        assert!(!boxed.to_string().is_empty());
    }

    /// `SystemTime` duration arithmetic mirrors the `Instant` side.
    #[test]
    fn system_time_arithmetic_round_trips() {
        let t = SystemTime::from_js_millis(1_700_000_000_000.0);
        let mut u = t;
        u += Duration::from_secs(10);
        assert_eq!(u, t + Duration::from_secs(10));
        u -= Duration::from_secs(10);
        assert_eq!(u, t);
        assert_eq!(
            t - Duration::from_secs(1),
            t.checked_sub(Duration::from_secs(1)).expect("in range")
        );
        assert_eq!(UNIX_EPOCH.checked_sub(Duration::from_secs(1)), None);
        assert_eq!(t.checked_add(Duration::MAX), None);
    }

    /// A clock that never advances (a suspended tab, or a browser clamping for
    /// Spectre mitigation) produces zero-length spans, not negative ones.
    #[test]
    fn a_stalled_clock_produces_zero_spans() {
        let a = Instant::from_js_millis(4242.0);
        let b = Instant::from_js_millis(4242.0);
        assert_eq!(b.duration_since(a), Duration::ZERO);
        assert_eq!(a, b);
        let s = SystemTime::from_js_millis(4242.0);
        let t = SystemTime::from_js_millis(4242.0);
        assert_eq!(t.duration_since(s).expect("equal times"), Duration::ZERO);
    }
}
