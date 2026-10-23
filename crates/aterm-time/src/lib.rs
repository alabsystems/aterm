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

#[cfg(any(all(target_arch = "wasm32", target_os = "unknown"), test))]
mod shim;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use shim::{Instant, SystemTime, SystemTimeError, UNIX_EPOCH};

/// Define the two browser clocks this crate reads on `wasm32-unknown-unknown`,
/// from the wasm module's own bindings of `performance.now()` and
/// `Date.now()`, each a `fn() -> f64`.
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
#[must_use]
#[inline]
pub fn since_epoch() -> Option<Duration> {
    let Ok(d) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return None;
    };
    Some(d)
}

/// Whole seconds since the Unix epoch ([`since_epoch`]), `None` before it.
#[must_use]
#[inline]
pub fn unix_secs() -> Option<u64> {
    since_epoch().map(|d| d.as_secs())
}

/// Milliseconds since the Unix epoch ([`since_epoch`]), saturating at
/// `u64::MAX`; `None` before it.
#[must_use]
#[inline]
pub fn unix_millis() -> Option<u64> {
    since_epoch().map(duration_to_millis_saturating)
}

/// Nanoseconds per second (`1_000_000_000`).
pub const NANOS_PER_SEC: u32 = 1_000_000_000;
/// Nanoseconds per millisecond (`1_000_000`).
pub const NANOS_PER_MILLI: u32 = 1_000_000;
/// Nanoseconds per microsecond (`1_000`).
pub const NANOS_PER_MICRO: u32 = 1_000;
/// Milliseconds per second (`1_000`).
pub const MILLIS_PER_SEC: u64 = 1_000;
/// Microseconds per second (`1_000_000`).
pub const MICROS_PER_SEC: u64 = 1_000_000;

/// Convert a [`Duration`] to whole milliseconds, saturating at `u64::MAX`.
#[must_use]
#[inline]
pub const fn duration_to_millis_saturating(d: Duration) -> u64 {
    let secs_ms = d.as_secs().saturating_mul(MILLIS_PER_SEC);
    let sub_ms = (d.subsec_nanos() / NANOS_PER_MILLI) as u64;
    secs_ms.saturating_add(sub_ms)
}

/// Convert a [`Duration`] to whole microseconds, saturating at `u64::MAX`.
#[must_use]
#[inline]
pub const fn duration_to_micros_saturating(d: Duration) -> u64 {
    let secs_us = d.as_secs().saturating_mul(MICROS_PER_SEC);
    let sub_us = (d.subsec_nanos() / NANOS_PER_MICRO) as u64;
    secs_us.saturating_add(sub_us)
}

/// Split whole milliseconds since epoch into `(secs, subsec_nanos)` where
/// `subsec_nanos < 1_000_000_000`.
#[must_use]
#[inline]
pub const fn split_millis_to_secs_nanos(millis: u64) -> (u64, u32) {
    let secs = millis / MILLIS_PER_SEC;
    let rem_ms = (millis % MILLIS_PER_SEC) as u32;
    (secs, rem_ms * NANOS_PER_MILLI)
}

/// Split whole microseconds since epoch into `(secs, subsec_nanos)` where
/// `subsec_nanos < 1_000_000_000`.
#[must_use]
#[inline]
pub const fn split_micros_to_secs_nanos(micros: u64) -> (u64, u32) {
    let secs = micros / MICROS_PER_SEC;
    let rem_us = (micros % MICROS_PER_SEC) as u32;
    (secs, rem_us * NANOS_PER_MICRO)
}

/// Construct a [`Duration`] from `(secs, subsec_nanos)`, rejecting
/// non-canonical `subsec_nanos >= 1_000_000_000`.
#[must_use]
#[inline]
pub const fn duration_from_canonical_parts(secs: u64, subsec_nanos: u32) -> Option<Duration> {
    if subsec_nanos >= NANOS_PER_SEC {
        return None;
    }
    Some(Duration::new(secs, subsec_nanos))
}

/// Encode a canonical `(secs: u64, subsec_nanos: u32)` duration/timestamp into
/// a 12-byte little-endian wire representation.
#[must_use]
#[inline]
pub const fn encode_timestamp_96_le(secs: u64, subsec_nanos: u32) -> Option<[u8; 12]> {
    if subsec_nanos >= NANOS_PER_SEC {
        return None;
    }
    Some([
        secs as u8,
        (secs >> 8) as u8,
        (secs >> 16) as u8,
        (secs >> 24) as u8,
        (secs >> 32) as u8,
        (secs >> 40) as u8,
        (secs >> 48) as u8,
        (secs >> 56) as u8,
        subsec_nanos as u8,
        (subsec_nanos >> 8) as u8,
        (subsec_nanos >> 16) as u8,
        (subsec_nanos >> 24) as u8,
    ])
}

/// Decode a 12-byte little-endian `(secs: u64, subsec_nanos: u32)` wire
/// timestamp, rejecting non-canonical `subsec_nanos >= 1_000_000_000`.
#[must_use]
#[inline]
pub const fn decode_timestamp_96_le(wire: &[u8; 12]) -> Option<(u64, u32)> {
    let secs = (wire[0] as u64)
        | ((wire[1] as u64) << 8)
        | ((wire[2] as u64) << 16)
        | ((wire[3] as u64) << 24)
        | ((wire[4] as u64) << 32)
        | ((wire[5] as u64) << 40)
        | ((wire[6] as u64) << 48)
        | ((wire[7] as u64) << 56);
    let nanos = (wire[8] as u32)
        | ((wire[9] as u32) << 8)
        | ((wire[10] as u32) << 16)
        | ((wire[11] as u32) << 24);
    if nanos >= NANOS_PER_SEC {
        return None;
    }
    Some((secs, nanos))
}

/// Encode a canonical `(secs: u64, subsec_nanos: u32)` duration/timestamp into
/// a 12-byte big-endian wire representation.
#[must_use]
#[inline]
pub const fn encode_timestamp_96_be(secs: u64, subsec_nanos: u32) -> Option<[u8; 12]> {
    if subsec_nanos >= NANOS_PER_SEC {
        return None;
    }
    Some([
        (secs >> 56) as u8,
        (secs >> 48) as u8,
        (secs >> 40) as u8,
        (secs >> 32) as u8,
        (secs >> 24) as u8,
        (secs >> 16) as u8,
        (secs >> 8) as u8,
        secs as u8,
        (subsec_nanos >> 24) as u8,
        (subsec_nanos >> 16) as u8,
        (subsec_nanos >> 8) as u8,
        subsec_nanos as u8,
    ])
}

/// Decode a 12-byte big-endian `(secs: u64, subsec_nanos: u32)` wire
/// timestamp, rejecting non-canonical `subsec_nanos >= 1_000_000_000`.
#[must_use]
#[inline]
pub const fn decode_timestamp_96_be(wire: &[u8; 12]) -> Option<(u64, u32)> {
    let secs = ((wire[0] as u64) << 56)
        | ((wire[1] as u64) << 48)
        | ((wire[2] as u64) << 40)
        | ((wire[3] as u64) << 32)
        | ((wire[4] as u64) << 24)
        | ((wire[5] as u64) << 16)
        | ((wire[6] as u64) << 8)
        | (wire[7] as u64);
    let nanos = ((wire[8] as u32) << 24)
        | ((wire[9] as u32) << 16)
        | ((wire[10] as u32) << 8)
        | (wire[11] as u32);
    if nanos >= NANOS_PER_SEC {
        return None;
    }
    Some((secs, nanos))
}

/// Advance a monotonic millisecond watermark `last_ms` given an observed
/// `candidate_ms`, never moving backward.
#[must_use]
#[inline]
pub const fn monotonic_watermark_ms(last_ms: u64, candidate_ms: u64) -> u64 {
    if candidate_ms > last_ms {
        candidate_ms
    } else {
        last_ms
    }
}

/// Remaining milliseconds until `deadline_ms` at `now_ms` (`0` if expired).
#[must_use]
#[inline]
pub const fn deadline_remaining_ms(now_ms: u64, deadline_ms: u64) -> u64 {
    deadline_ms.saturating_sub(now_ms)
}

#[cfg(test)]
mod tests {
    use super::{
        NANOS_PER_SEC, SystemTime, UNIX_EPOCH, deadline_remaining_ms, decode_timestamp_96_be,
        decode_timestamp_96_le, duration_from_canonical_parts, duration_to_micros_saturating,
        duration_to_millis_saturating, encode_timestamp_96_be, encode_timestamp_96_le,
        monotonic_watermark_ms, split_micros_to_secs_nanos, split_millis_to_secs_nanos,
    };

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

    #[test]
    fn timestamp_96_wire_codecs_and_monotonic_helpers_round_trip() {
        let (secs, nanos) = split_millis_to_secs_nanos(1_729_281_234_567);
        assert_eq!(secs, 1_729_281_234);
        assert_eq!(nanos, 567_000_000);
        let d = duration_from_canonical_parts(secs, nanos).expect("canonical");
        assert_eq!(duration_to_millis_saturating(d), 1_729_281_234_567);
        assert_eq!(duration_to_micros_saturating(d), 1_729_281_234_567_000);
        assert_eq!(
            split_micros_to_secs_nanos(1_729_281_234_567_890),
            (1_729_281_234, 567_890_000)
        );

        let le = encode_timestamp_96_le(secs, nanos).expect("canonical le");
        assert_eq!(decode_timestamp_96_le(&le), Some((secs, nanos)));
        let be = encode_timestamp_96_be(secs, nanos).expect("canonical be");
        assert_eq!(decode_timestamp_96_be(&be), Some((secs, nanos)));

        assert!(encode_timestamp_96_le(1, NANOS_PER_SEC).is_none());
        assert!(encode_timestamp_96_be(1, NANOS_PER_SEC).is_none());
        assert!(duration_from_canonical_parts(1, NANOS_PER_SEC).is_none());

        assert_eq!(monotonic_watermark_ms(100, 90), 100);
        assert_eq!(monotonic_watermark_ms(100, 120), 120);
        assert_eq!(deadline_remaining_ms(80, 100), 20);
        assert_eq!(deadline_remaining_ms(120, 100), 0);
    }
}
