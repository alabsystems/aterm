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
/// `saturating_mul`: the u128 product of two values `< 2^64` cannot saturate,
/// but the plain multiply's 128-bit overflow obligation makes the strict gate's
/// bit-vector solver hang, while the saturating form has no panic path.
pub(crate) fn threshold_bytes(percent: usize, budget: usize) -> usize {
    let threshold = (budget as u128).saturating_mul(percent as u128) / 100;
    // The quotient is <= budget (callers clamp percent to 1..=100), so the `min`
    // never changes it; it makes the narrowing cast provably lossless for the
    // strict gate, which cannot carry the division bound.
    threshold.min(usize::MAX as u128) as usize
}

/// Recompute watermark level from current budgeted bytes vs thresholds.
///
/// Used by [`Scrollback`]'s accounting sync. Hysteresis: Yellow→Green requires
/// dropping below `yellow_exit_threshold`, not just `yellow_threshold`.
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
