// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Watermark policy types and threshold helpers for scrollback.

/// Memory pressure watermark level for scrollback backpressure.
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
#[inline]
pub(crate) const fn threshold_bytes(percent: usize, budget: usize) -> usize {
    let threshold = (budget as u128).saturating_mul(percent as u128) / 100;
    let clamped = if threshold > usize::MAX as u128 {
        usize::MAX as u128
    } else {
        threshold
    };
    clamped as usize
}

/// Recompute watermark level from current budgeted bytes vs thresholds.
#[inline]
pub(crate) const fn recompute_watermark(
    current: WatermarkLevel,
    budgeted_bytes: usize,
    red_threshold: usize,
    yellow_threshold: usize,
    yellow_exit_threshold: usize,
) -> WatermarkLevel {
    if budgeted_bytes >= red_threshold {
        WatermarkLevel::Red
    } else if budgeted_bytes >= yellow_threshold {
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
