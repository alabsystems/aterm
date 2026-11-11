// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shared time utilities for pipeline timing.

use core::time::Duration;

/// Convert a [`Duration`] to nanoseconds as `u64`, saturating at `u64::MAX`.
#[must_use]
#[inline]
// Skip: `Duration` is `core::time::Duration`, and the arithmetic below is
// saturating. Measured without this skip on trust seal 321aaeda7:
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
#[path = "time_tests.rs"]
mod tests;
