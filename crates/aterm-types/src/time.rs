// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shared time utilities for pipeline timing.

use core::time::Duration;

/// Convert a [`Duration`] to nanoseconds as `u64`, saturating at `u64::MAX`.
///
/// Uses `as_secs()` + `subsec_nanos()` instead of `as_nanos() as u64` to
/// avoid `clippy::cast_possible_truncation` on the `u128` → `u64` cast.
/// Saturates at `u64::MAX` (~584 years) which is unreachable in practice.
#[must_use]
#[inline]
// Skip: `Duration` is `core::time::Duration`, and the arithmetic below is
// saturating. Measured without this skip on trust seal 321aaeda7 (2026-09-27):
// the one obligation stays runtime-checked, because the native trust-mc
// bundle refuses the by-value `Duration` argument ("Alloca without an
// internally bound extent, pointee-type, and alignment derivation"). Needed
// from $HOME/trust, in a promoted seal: TrustIr lowering of a by-value struct
// argument's alloca; then drop this and re-run
// `targo trust check -p aterm-types --lib --function duration_to_nanos`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn duration_to_nanos(duration: Duration) -> u64 {
    duration
        .as_secs()
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::from(duration.subsec_nanos()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_to_nanos_cases() {
        for (what, duration, nanos) in [
            ("zero", Duration::ZERO, 0),
            ("one second", Duration::from_secs(1), 1_000_000_000),
            ("subsec nanos only", Duration::from_nanos(42), 42),
            (
                "mixed secs and nanos",
                Duration::new(2, 500_000_000),
                2_500_000_000,
            ),
            ("saturates at max", Duration::from_secs(u64::MAX), u64::MAX),
        ] {
            assert_eq!(duration_to_nanos(duration), nanos, "{what}");
        }
    }
}
