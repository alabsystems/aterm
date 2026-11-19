// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Platform clock facade: re-exports `std::time` on native targets and
//! bridges to browser clocks (`performance.now()` / `Date.now()`) on
//! `wasm32-unknown-unknown`, where `std::time::Instant::now()` panics.
//! Its wall-clock accessors count their reads per thread ([`raw_reads`]).
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

std::thread_local! {
    /// The calling thread's count of wall reads made by [`wall_now`]: the
    /// value [`raw_reads`] returns.
    static RAW_READS: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
}

/// One read of the platform wall clock, added to the calling thread's
/// [`raw_reads`] count.
#[inline]
fn wall_now() -> SystemTime {
    RAW_READS.with(|reads| reads.set(reads.get().wrapping_add(1)));
    let now = SystemTime::now();
    #[cfg(test)]
    tests::record_wall_read(now);
    now
}

/// How many times the calling thread has read the wall clock through
/// [`since_epoch`], [`unix_secs`] or [`unix_millis`], wrapping at `u64::MAX`.
///
/// A thread starts at zero, and each call of one of the three adds one. The
/// `*_at` conversions take a reading the caller already holds and add
/// nothing. A `SystemTime::now()` called directly is not counted either. No
/// other thread's reads move it, so two values taken on one thread, compared
/// with `wrapping_sub`, say whether the code that ran on that thread between
/// them read the wall clock through these accessors.
#[must_use]
#[inline]
pub fn raw_reads() -> u64 {
    RAW_READS.with(core::cell::Cell::get)
}

/// The wall clock's distance past the Unix epoch, or `None` when it reads
/// before 1970. Each call is one read in [`raw_reads`].
#[must_use]
#[inline]
pub fn since_epoch() -> Option<Duration> {
    since_epoch_at(wall_now())
}

/// Whole seconds since the Unix epoch ([`since_epoch`]), `None` before it.
/// Each call is one read in [`raw_reads`].
#[must_use]
#[inline]
pub fn unix_secs() -> Option<u64> {
    unix_secs_at(wall_now())
}

/// Milliseconds since the Unix epoch ([`since_epoch`]), saturating at
/// `u64::MAX`; `None` before it. Each call is one read in [`raw_reads`].
#[must_use]
#[inline]
pub fn unix_millis() -> Option<u64> {
    unix_millis_at(wall_now())
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
#[path = "lib_tests.rs"]
mod tests;
