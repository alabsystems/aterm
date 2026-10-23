// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `wasm32` clock: a monotonic [`Instant`] over `performance.now()` and a
//! wall-clock [`SystemTime`] over `Date.now()`.

use core::ops::{Add, AddAssign, Sub, SubAssign};
use core::time::Duration;

/// The browser's two clock functions. This crate binds no JS: they are
/// link-time symbols the wasm module defines with [`crate::wasm_clock_host!`]
/// from its own bindings, so the crate has no dependency. The signatures live
/// in that macro and here, and nowhere else.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod js {
    unsafe extern "C" {
        /// `performance.now()` in milliseconds since the time origin.
        #[link_name = "aterm_wasm_performance_now"]
        pub safe fn performance_now() -> f64;

        /// `Date.now()` in milliseconds since the Unix epoch.
        #[link_name = "aterm_wasm_date_now"]
        pub safe fn date_now() -> f64;
    }
}

/// Convert JS milliseconds to a [`Duration`], totally.
#[inline]
fn duration_from_millis_f64(ms: f64) -> Duration {
    if !ms.is_finite() || ms <= 0.0 {
        return Duration::ZERO;
    }
    let whole = ms.trunc();
    let frac = ms - whole;
    let millis = if whole >= u64::MAX as f64 {
        u64::MAX
    } else {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "guarded above: finite, > 0, and below u64::MAX"
        )]
        {
            whole as u64
        }
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "frac is in [0, 1), so the product is in [0, 1e6)"
    )]
    let nanos = (frac * 1_000_000.0) as u32;
    Duration::from_millis(millis).saturating_add(Duration::from_nanos(u64::from(nanos)))
}

/// Monotonic clock measurement (`wasm32` stand-in for `std::time::Instant`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Instant(Duration);

impl Instant {
    /// Read the monotonic clock.
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    #[must_use]
    #[inline]
    pub fn now() -> Self {
        Self::from_js_millis(js::performance_now())
    }

    /// Build from a raw JS millisecond reading: the seam the native tests drive.
    #[inline]
    fn from_js_millis(ms: f64) -> Self {
        Self(duration_from_millis_f64(ms))
    }

    /// Time elapsed from `earlier` to `self`, saturating to zero.
    #[must_use]
    #[inline]
    pub const fn duration_since(&self, earlier: Self) -> Duration {
        self.saturating_duration_since(earlier)
    }

    /// Time elapsed from `earlier` to `self`, or `None` if `earlier` is later.
    #[must_use]
    #[inline]
    pub const fn checked_duration_since(&self, earlier: Self) -> Option<Duration> {
        self.0.checked_sub(earlier.0)
    }

    /// Time elapsed from `earlier` to `self`, or zero if `earlier` is later.
    #[must_use]
    #[inline]
    pub const fn saturating_duration_since(&self, earlier: Self) -> Duration {
        self.0.saturating_sub(earlier.0)
    }

    /// Time elapsed since this instant was taken.
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    #[must_use]
    #[inline]
    pub fn elapsed(&self) -> Duration {
        Self::now().saturating_duration_since(*self)
    }

    /// `self + duration`, or `None` on overflow.
    #[must_use]
    #[inline]
    pub const fn checked_add(&self, duration: Duration) -> Option<Self> {
        match self.0.checked_add(duration) {
            Some(d) => Some(Self(d)),
            None => None,
        }
    }

    /// `self - duration`, or `None` if it would precede the time origin.
    #[must_use]
    #[inline]
    pub const fn checked_sub(&self, duration: Duration) -> Option<Self> {
        match self.0.checked_sub(duration) {
            Some(d) => Some(Self(d)),
            None => None,
        }
    }
}

impl Add<Duration> for Instant {
    type Output = Self;
    /// # Panics
    /// On overflow, exactly like `std::time::Instant`.
    #[inline]
    fn add(self, rhs: Duration) -> Self {
        self.checked_add(rhs)
            .expect("overflow when adding duration to instant")
    }
}

impl AddAssign<Duration> for Instant {
    #[inline]
    fn add_assign(&mut self, rhs: Duration) {
        *self = *self + rhs;
    }
}

impl Sub<Duration> for Instant {
    type Output = Self;
    /// # Panics
    /// On underflow, exactly like `std::time::Instant`.
    #[inline]
    fn sub(self, rhs: Duration) -> Self {
        self.checked_sub(rhs)
            .expect("overflow when subtracting duration from instant")
    }
}

impl SubAssign<Duration> for Instant {
    #[inline]
    fn sub_assign(&mut self, rhs: Duration) {
        *self = *self - rhs;
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;
    /// Saturating, matching `std`'s `Instant - Instant`.
    #[inline]
    fn sub(self, rhs: Self) -> Duration {
        self.saturating_duration_since(rhs)
    }
}

/// The anchor of [`SystemTime`]: 1970-01-01 00:00:00 UTC.
pub const UNIX_EPOCH: SystemTime = SystemTime(Duration::ZERO);

/// Wall-clock measurement (`wasm32` stand-in for `std::time::SystemTime`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SystemTime(Duration);

impl SystemTime {
    /// The anchor, as an associated constant (the `std` spelling).
    pub const UNIX_EPOCH: Self = UNIX_EPOCH;

    /// Read the wall clock.
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    #[must_use]
    #[inline]
    pub fn now() -> Self {
        Self::from_js_millis(js::date_now())
    }

    /// Build from a raw JS millisecond reading: the native tests' seam.
    #[inline]
    fn from_js_millis(ms: f64) -> Self {
        Self(duration_from_millis_f64(ms))
    }

    /// Time elapsed from `earlier` to `self`.
    ///
    /// # Errors
    /// Returns an error when `earlier` is later than `self`.
    #[inline]
    pub const fn duration_since(&self, earlier: Self) -> Result<Duration, SystemTimeError> {
        match self.0.checked_sub(earlier.0) {
            Some(d) => Ok(d),
            None => Err(SystemTimeError(earlier.0.saturating_sub(self.0))),
        }
    }

    /// Time elapsed since this measurement.
    ///
    /// # Errors
    /// Returns an error when the wall clock has moved backwards since `self` was taken.
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    #[inline]
    pub fn elapsed(&self) -> Result<Duration, SystemTimeError> {
        Self::now().duration_since(*self)
    }

    /// `self + duration`, or `None` on overflow.
    #[must_use]
    #[inline]
    pub const fn checked_add(&self, duration: Duration) -> Option<Self> {
        match self.0.checked_add(duration) {
            Some(d) => Some(Self(d)),
            None => None,
        }
    }

    /// `self - duration`, or `None` if it would precede the epoch.
    #[must_use]
    #[inline]
    pub const fn checked_sub(&self, duration: Duration) -> Option<Self> {
        match self.0.checked_sub(duration) {
            Some(d) => Some(Self(d)),
            None => None,
        }
    }
}

impl Add<Duration> for SystemTime {
    type Output = Self;
    /// # Panics
    /// On overflow, exactly like `std::time::SystemTime`.
    #[inline]
    fn add(self, rhs: Duration) -> Self {
        self.checked_add(rhs)
            .expect("overflow when adding duration to system time")
    }
}

impl AddAssign<Duration> for SystemTime {
    #[inline]
    fn add_assign(&mut self, rhs: Duration) {
        *self = *self + rhs;
    }
}

impl Sub<Duration> for SystemTime {
    type Output = Self;
    /// # Panics
    /// On underflow, exactly like `std::time::SystemTime`.
    #[inline]
    fn sub(self, rhs: Duration) -> Self {
        self.checked_sub(rhs)
            .expect("overflow when subtracting duration from system time")
    }
}

impl SubAssign<Duration> for SystemTime {
    #[inline]
    fn sub_assign(&mut self, rhs: Duration) {
        *self = *self - rhs;
    }
}

/// Error returned by [`SystemTime::duration_since`] when the wall clock ran backwards.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SystemTimeError(Duration);

impl SystemTimeError {
    /// How far the second time is ahead of the first.
    #[must_use]
    #[inline]
    pub const fn duration(&self) -> Duration {
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

    /// The clock conversion is total: every `f64` a browser binding could
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
        assert_eq!(
            duration_from_millis_f64(f64::MAX),
            Duration::from_millis(u64::MAX)
        );
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

    /// Sub-microsecond resolution survives an hour-old time origin, so two
    /// frames stay distinguishable.
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

    /// `SystemTime` is anchored at the epoch, and a backwards step is an error
    /// carrying the magnitude, never a panic or a wrap.
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
}
