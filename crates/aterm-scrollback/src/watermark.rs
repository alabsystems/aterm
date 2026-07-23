// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Watermark policy types and threshold helpers for scrollback.

/// Memory pressure watermark level for scrollback backpressure.
///
/// Consumers query this to throttle input when scrollback is under memory pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[non_exhaustive]
pub enum WatermarkLevel {
    /// Below yellow threshold. No pressure.
    #[default]
    Green,
    /// At or above yellow threshold (default 80%). Eager compression active.
    Yellow,
    /// At or above red threshold (default 95%). Backpressure recommended.
    Red,
}

/// Default yellow watermark as percentage of memory budget.
pub(crate) const DEFAULT_YELLOW_PERCENT: usize = 80;

/// Default red watermark as percentage of memory budget.
pub(crate) const DEFAULT_RED_PERCENT: usize = 95;

/// Hysteresis exit: Yellow drops to Green when below this percentage.
pub(crate) const YELLOW_EXIT_PERCENT: usize = 50;

/// Compute an absolute byte threshold from a percentage and a budget.
///
/// `saturating_mul` instead of `*`: both operands are `< 2^64`, so the u128
/// product can never saturate and the result is identical — but the plain
/// multiply carries a 128-bit overflow obligation whose bit-vector encoding
/// the strict gate's solver grinds on without terminating (observed as a
/// multi-CPU-minute hang), while the saturating form has no panic path at all.
pub(crate) fn threshold_bytes(percent: usize, budget: usize) -> usize {
    let threshold = (budget as u128).saturating_mul(percent as u128) / 100;
    // `.min(usize::MAX)` before the narrowing cast: the quotient is always
    // <= budget (callers clamp percent to 1..=100) so the clamp can never
    // change the value — it just makes the cast provably lossless for the
    // strict gate, which cannot carry the division bound.
    threshold.min(usize::MAX as u128) as usize
}

/// Recompute watermark level from current budgeted bytes vs thresholds.
///
/// Shared implementation for [`Scrollback`] and [`DiskBackedScrollback`].
/// Uses hysteresis: Yellow→Green requires dropping below `yellow_exit_threshold`,
/// not just below `yellow_threshold`.
#[inline]
pub(crate) fn recompute_watermark(
    current: WatermarkLevel,
    budgeted_bytes: usize,
    red_threshold: usize,
    yellow_threshold: usize,
    yellow_exit_threshold: usize,
) -> WatermarkLevel {
    if budgeted_bytes >= red_threshold {
        WatermarkLevel::Red
    } else if budgeted_bytes >= yellow_threshold {
        // Between yellow and red thresholds: clamp to Yellow regardless of
        // prior level (Green→Yellow, Red→Yellow, Yellow stays Yellow).
        WatermarkLevel::Yellow
    } else {
        match current {
            WatermarkLevel::Red => WatermarkLevel::Yellow,
            WatermarkLevel::Yellow => {
                if budgeted_bytes < yellow_exit_threshold {
                    WatermarkLevel::Green
                } else {
                    WatermarkLevel::Yellow
                }
            }
            WatermarkLevel::Green => WatermarkLevel::Green,
        }
    }
}
