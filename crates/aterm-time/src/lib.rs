// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Platform clock facade: re-exports `std::time` on native targets and
//! bridges to browser clocks (`performance.now()` / `Date.now()`) on
//! `wasm32-unknown-unknown`, where `std::time::Instant::now()` panics.
#![cfg_attr(
    not(all(target_arch = "wasm32", target_os = "unknown")),
    forbid(unsafe_code)
)]

/// A span of time, platform-independent: `core::time::Duration`, re-exported
/// so callers need only one import.
pub use core::time::Duration;

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod native {
    /// The platform monotonic clock: `std::time::Instant` itself.
    pub use std::time::Instant;
    /// The platform wall clock: `std::time::SystemTime` itself.
    pub use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use native::{Instant, SystemTime, SystemTimeError, UNIX_EPOCH};

#[cfg(any(all(target_arch = "wasm32", target_os = "unknown"), trust_verify, test))]
mod shim;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use shim::{Instant, SystemTime, SystemTimeError, UNIX_EPOCH};

/// Define the two browser clocks this crate reads on `wasm32-unknown-unknown`,
/// from the wasm module's own bindings of `performance.now()` and
/// `Date.now()`, each a `fn() -> f64`.
///
/// Invoke it once, in the crate that builds the `.wasm` (the cdylib). It
/// expands to two `#[unsafe(no_mangle)] extern "C"` functions named
/// `aterm_wasm_performance_now` and `aterm_wasm_date_now`. Each is gated to
/// `wasm32-unknown-unknown`, so the invocation compiles to nothing on every
/// other target, natively included. Being `no_mangle` in a cdylib, both are
/// also exports of the module.
///
/// ```
/// fn performance_now() -> f64 {
///     0.0
/// }
/// fn date_now() -> f64 {
///     0.0
/// }
/// aterm_time::wasm_clock_host!(performance_now, date_now);
/// ```
#[macro_export]
macro_rules! wasm_clock_host {
    ($performance_now:path, $date_now:path $(,)?) => {
        /// `performance.now()`, for `aterm_time::Instant`.
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        #[unsafe(no_mangle)]
        extern "C" fn aterm_wasm_performance_now() -> f64 {
            $performance_now()
        }

        /// `Date.now()`, for `aterm_time::SystemTime`.
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        #[unsafe(no_mangle)]
        extern "C" fn aterm_wasm_date_now() -> f64 {
            $date_now()
        }
    };
}

/// Distance past [`UNIX_EPOCH`] for an explicit [`SystemTime`] reading, or
/// `None` when `now` precedes 1970.
#[must_use]
#[inline]
pub fn since_epoch_at(now: SystemTime) -> Option<Duration> {
    now.duration_since(UNIX_EPOCH).ok()
}

/// Whole seconds since [`UNIX_EPOCH`] for `now` ([`since_epoch_at`]), `None`
/// before it.
#[must_use]
#[inline]
pub fn unix_secs_at(now: SystemTime) -> Option<u64> {
    since_epoch_at(now).map(|d| d.as_secs())
}

/// Milliseconds since [`UNIX_EPOCH`] for `now` ([`since_epoch_at`]),
/// saturating at `u64::MAX`; `None` before it.
#[must_use]
#[inline]
pub fn unix_millis_at(now: SystemTime) -> Option<u64> {
    since_epoch_at(now).map(duration_to_millis_saturating)
}

/// The wall clock's distance past the Unix epoch, or `None` when it reads
/// before 1970.
#[must_use]
#[inline]
pub fn since_epoch() -> Option<Duration> {
    since_epoch_at(SystemTime::now())
}

/// Whole seconds since the Unix epoch ([`since_epoch`]), `None` before it.
#[must_use]
#[inline]
pub fn unix_secs() -> Option<u64> {
    unix_secs_at(SystemTime::now())
}

/// Milliseconds since the Unix epoch ([`since_epoch`]), saturating at
/// `u64::MAX`; `None` before it.
#[must_use]
#[inline]
pub fn unix_millis() -> Option<u64> {
    unix_millis_at(SystemTime::now())
}

/// Nanoseconds per millisecond (`1_000_000`).
pub const NANOS_PER_MILLI: u32 = 1_000_000;
/// Milliseconds per second (`1_000`).
pub const MILLIS_PER_SEC: u64 = 1_000;

/// Convert a [`Duration`] to whole milliseconds, saturating at `u64::MAX`.
#[must_use]
#[inline]
pub const fn duration_to_millis_saturating(d: Duration) -> u64 {
    let secs_ms = d.as_secs().saturating_mul(MILLIS_PER_SEC);
    let sub_ms = (d.subsec_nanos() / NANOS_PER_MILLI) as u64;
    secs_ms.saturating_add(sub_ms)
}

#[cfg(test)]
mod tests {
    use super::{Duration, SystemTime, UNIX_EPOCH};

    /// Pure epoch conversion agrees across all three units on one reading,
    /// rejects pre-epoch readings, saturates milliseconds at `u64::MAX`, and
    /// reads post-epoch values from the live wall clock without assuming
    /// monotonicity across separate syscalls.
    #[test]
    fn the_epoch_accessors_read_the_wall_clock() {
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        {
            let before_epoch = UNIX_EPOCH
                .checked_sub(Duration::from_secs(1))
                .expect("native SystemTime represents 1969");
            assert_eq!(super::since_epoch_at(before_epoch), None);
            assert_eq!(super::unix_secs_at(before_epoch), None);
            assert_eq!(super::unix_millis_at(before_epoch), None);
        }

        assert_eq!(super::since_epoch_at(UNIX_EPOCH), Some(Duration::ZERO));
        assert_eq!(super::unix_secs_at(UNIX_EPOCH), Some(0));
        assert_eq!(super::unix_millis_at(UNIX_EPOCH), Some(0));

        let sample = Duration::new(1_790_000_000, 123_456_789);
        let at_sample = UNIX_EPOCH.checked_add(sample).expect("post-epoch sample");
        assert_eq!(super::since_epoch_at(at_sample), Some(sample));
        assert_eq!(super::unix_secs_at(at_sample), Some(1_790_000_000));
        assert_eq!(super::unix_millis_at(at_sample), Some(1_790_000_000_123));

        assert_eq!(
            super::duration_to_millis_saturating(Duration::from_secs(u64::MAX)),
            u64::MAX
        );

        let now = SystemTime::now();
        let d = super::since_epoch_at(now).expect("wall clock is at or after the Unix epoch");
        assert_eq!(super::unix_secs_at(now), Some(d.as_secs()));
        assert_eq!(
            super::unix_millis_at(now),
            Some(super::duration_to_millis_saturating(d))
        );
        assert!(super::since_epoch().is_some());
        assert!(super::unix_secs().is_some());
        assert!(super::unix_millis().is_some());
    }
}
