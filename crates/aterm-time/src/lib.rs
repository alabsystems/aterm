// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm's clock: `std::time` everywhere it exists, and a browser clock where
//! it does not.
//!
//! `std::time::Instant::now()` panics on `wasm32-unknown-unknown` ("time not
//! implemented on this platform"): the target has no clock syscall and the
//! browser's clocks are JS functions. The engine samples a monotonic clock
//! everywhere (VT processing, the effects tick, predictive echo, present
//! pacing), so a wasm build would trap on its first frame.
//!
//! On every target with `std::time`, this crate is a plain re-export:
//! [`Instant`] is `std::time::Instant`. On `wasm32-unknown-unknown` it swaps
//! in an [`Instant`] over `performance.now()` (monotonic, sub-millisecond) and
//! a [`SystemTime`] over `Date.now()` (wall clock, ms since the Unix epoch).
//! [`Duration`] is `core::time::Duration` on both arms.
//!
//! [`since_epoch`], [`unix_secs`] and [`unix_millis`] are the workspace's one
//! spelling of "the wall clock as a Unix timestamp", correct on both arms.
//!
//! # Precision on the wasm arm
//!
//! Both browser clocks return `f64` milliseconds, which may be negative,
//! non-finite or clamped. The conversion is total (non-finite and negative
//! floor to zero) and splits whole milliseconds from the fraction before
//! scaling, so an old time origin keeps sub-microsecond resolution.
//!
//! # Where the browser clocks come from
//!
//! This crate binds no JS and has no dependency. On `wasm32-unknown-unknown`
//! it reads two link-time symbols, `aterm_wasm_performance_now` and
//! `aterm_wasm_date_now`, and the wasm module defines them from its own JS
//! bindings with [`wasm_clock_host!`]. The module (a cdylib) chooses how JS is
//! reached, so the clock is the same whichever binding layer the module uses.
//!
//! A module that never invokes the macro does not link: `rust-lld` stops at
//! `undefined symbol: aterm_wasm_date_now` (measured on rustup stable 1.96.0,
//! wasm-platform WP.4). There is no install order to get wrong and no runtime
//! fallback.
//!
//! # Testing the wasm arm
//!
//! The Trust toolchain has no `wasm32` std, so the `wasm-cpu` and `wasm-gpu`
//! cells of `verify cells` type-check the wasm arm on rustup's `stable`.
//! A box without the `wasm32` std reports them skipped, never passed.
//! `verify wasm-smoke` runs both clocks in the two web modules under
//! node. The shim's arithmetic is also compiled into the native test build
//! (see `shim`).

// A clock needs no unsafe; on the wasm arm the one `unsafe` is the extern
// block naming the two clock symbols.
#![cfg_attr(
    not(all(target_arch = "wasm32", target_os = "unknown")),
    forbid(unsafe_code)
)]

/// A span of time, platform-independent: `core::time::Duration`, re-exported
/// so callers need only one import.
pub use core::time::Duration;

// The arm predicate is exactly `wasm32-unknown-unknown`, not `target_arch =
// "wasm32"`: `wasm32-wasip1` and `wasm32-unknown-emscripten` have a working
// `std::time` and no `performance` object.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod native {
    /// The platform monotonic clock: `std::time::Instant` itself.
    pub use std::time::Instant;
    /// The platform wall clock: `std::time::SystemTime` itself.
    pub use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use native::{Instant, SystemTime, SystemTimeError, UNIX_EPOCH};

#[cfg(any(all(target_arch = "wasm32", target_os = "unknown"), test))]
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

/// The wall clock's distance past the Unix epoch, or `None` when it reads
/// before 1970.
///
/// `SystemTime::now().duration_since(UNIX_EPOCH)` through this crate's
/// [`SystemTime`], so it is also correct on `wasm32-unknown-unknown`. A clock
/// before the epoch has no one right answer, so each caller writes its own
/// fallback.
#[must_use]
pub fn since_epoch() -> Option<Duration> {
    SystemTime::now().duration_since(UNIX_EPOCH).ok()
}

/// Whole seconds since the Unix epoch ([`since_epoch`]), `None` before it.
#[must_use]
pub fn unix_secs() -> Option<u64> {
    since_epoch().map(|d| d.as_secs())
}

/// Milliseconds since the Unix epoch ([`since_epoch`]), saturating at
/// `u64::MAX`; `None` before it.
#[must_use]
pub fn unix_millis() -> Option<u64> {
    since_epoch().map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::{SystemTime, UNIX_EPOCH};

    /// The accessors are the same reading in three units, bracketed by two
    /// direct reads taken around them.
    #[test]
    fn the_epoch_accessors_read_the_wall_clock() {
        let direct = || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("wall clock is at or after the Unix epoch")
        };
        let before = direct();
        let (since, secs, millis) = (
            super::since_epoch().expect("after the epoch"),
            super::unix_secs().expect("after the epoch"),
            super::unix_millis().expect("after the epoch"),
        );
        let after = direct();
        assert!(
            (before..=after).contains(&since),
            "{before:?} {since:?} {after:?}"
        );
        assert!((before.as_secs()..=after.as_secs()).contains(&secs));
        assert!((before.as_millis()..=after.as_millis()).contains(&u128::from(millis)));
    }
}
