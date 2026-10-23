// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]
#![warn(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! Tiered scrollback storage for terminal emulators.

#![warn(missing_docs)]

mod access;
mod codec;
mod cold_tier;
mod error;
mod hot_tier;
mod iter;
mod line;
mod log_shim;
pub(crate) mod lz4;
mod scrollback_accounting;
mod storage;
mod tier;
mod tier_capabilities;
mod tier_ops;
mod watermark;

pub use access::ScrollbackAccess;
pub use aterm_rle::Rle;
pub(crate) use codec::{
    DEFAULT_BLOCK_SIZE, DEFAULT_HOT_LIMIT, DEFAULT_WARM_LIMIT, binary_search_counted,
    decode_cold_bounded, decompress_lz4_bounded, encode_cold_block,
};
pub use codec::{DEFAULT_LINE_LIMIT, DEFAULT_MEMORY_BUDGET};
pub(crate) use cold_tier::ColdTier;
pub use error::ScrollbackError;
pub(crate) use hot_tier::HotTier;
pub use iter::{DenseScrollbackIter, ScrollbackIter};
pub use line::{AttrRunCursor, CellAttrs, HyperlinkSpan, ImageSpan, Line, UnderlineColorSpan};
pub use line::{
    deserialize_lines, deserialize_lines_strict, deserialize_lines_strict_dropping_over_cap_links,
    deserialize_lines_tail_strict, max_linked_record_bytes, serialize_lines,
    serialize_lines_bounded,
};
pub use storage::ScrollbackStorage;
pub(crate) use tier::WarmTier;
pub use tier_capabilities::{ColdTierCodec, TierCapabilities};
pub use watermark::WatermarkLevel;
pub(crate) use watermark::{
    DEFAULT_RED_PERCENT, DEFAULT_YELLOW_PERCENT, YELLOW_EXIT_PERCENT, recompute_watermark,
    threshold_bytes,
};

/// Scrollback buffer with tiered storage.
#[derive(Debug)]
pub struct Scrollback {
    /// Hot tier: uncompressed lines (instant access).
    hot: HotTier,
    /// Warm tier: LZ4 compressed blocks.
    warm: WarmTier,
    /// Cold tier: LZ4 compressed pages, in-memory.
    cold: ColdTier,
    /// Maximum lines in hot tier before promotion.
    hot_limit: usize,
    /// Maximum lines in warm tier before eviction.
    warm_limit: usize,
    /// Total memory budget (bytes).
    memory_budget: usize,
    /// Lines per compressed block.
    block_size: usize,
    /// Total line count across all tiers.
    line_count: usize,
    /// Running diagnostic total for `total_memory_used()` (includes cache + overhead).
    bytes_used: usize,
    /// Reclaimable storage bytes for budget enforcement (excludes cache + overhead).
    budgeted_bytes: usize,
    /// Maximum total lines; older lines are discarded past it (None = no limit).
    line_limit: Option<usize>,
    /// Monotonic count of lines dropped by memory-pressure eviction (cold FIFO
    /// under the byte budget) or lost with a quarantined warm block.
    pressure_evicted_lines: u64,
    /// Monotonic count of history lines that crossed the image retention
    /// horizon; see [`Self::image_rows_dropped_by_compression`].
    image_rows_dropped_by_compression: u64,
    /// Current memory pressure watermark level.
    watermark_level: WatermarkLevel,
    /// Absolute byte threshold for Yellow level (entry).
    yellow_threshold: usize,
    /// Absolute byte threshold for exiting Yellow back to Green (hysteresis).
    yellow_exit_threshold: usize,
    /// Absolute byte threshold for Red level.
    red_threshold: usize,
}

impl Scrollback {
    /// Create a new scrollback buffer with the given hot and warm line limits and memory budget (bytes).
    #[must_use]
    pub fn new(hot_limit: usize, warm_limit: usize, memory_budget: usize) -> Self {
        let hot = HotTier::new();
        let warm = WarmTier::new();
        let cold = ColdTier::new();
        Self {
            bytes_used: hot
                .memory_used()
                .saturating_add(warm.memory_used())
                .saturating_add(cold.compressed_size()),
            budgeted_bytes: 0,
            hot,
            warm,
            cold,
            hot_limit: hot_limit.max(1),
            warm_limit,
            memory_budget,
            block_size: DEFAULT_BLOCK_SIZE,
            line_count: 0,
            line_limit: Some(DEFAULT_LINE_LIMIT),
            pressure_evicted_lines: 0,
            image_rows_dropped_by_compression: 0,
            watermark_level: WatermarkLevel::Green,
            yellow_threshold: threshold_bytes(DEFAULT_YELLOW_PERCENT, memory_budget),
            yellow_exit_threshold: threshold_bytes(YELLOW_EXIT_PERCENT, memory_budget),
            red_threshold: threshold_bytes(DEFAULT_RED_PERCENT, memory_budget),
        }
    }

    /// Create a scrollback buffer with the defaults.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_HOT_LIMIT, DEFAULT_WARM_LIMIT, DEFAULT_MEMORY_BUDGET)
    }

    /// Create a scrollback buffer with a custom block size, clamped to `1..=min(hot_limit, MAX_DECODE_PAGE_LINES)`.
    #[must_use]
    pub fn with_block_size(
        hot_limit: usize,
        warm_limit: usize,
        memory_budget: usize,
        block_size: usize,
    ) -> Self {
        let hot_limit = hot_limit.max(1);
        let block_size = block_size
            .max(1)
            .min(hot_limit)
            .min(crate::line::MAX_DECODE_PAGE_LINES);
        let hot = HotTier::new();
        let warm = WarmTier::new();
        let cold = ColdTier::new();
        Self {
            bytes_used: hot
                .memory_used()
                .saturating_add(warm.memory_used())
                .saturating_add(cold.compressed_size()),
            budgeted_bytes: 0,
            hot,
            warm,
            cold,
            hot_limit,
            warm_limit,
            memory_budget,
            block_size,
            line_count: 0,
            line_limit: Some(DEFAULT_LINE_LIMIT),
            pressure_evicted_lines: 0,
            image_rows_dropped_by_compression: 0,
            watermark_level: WatermarkLevel::Green,
            yellow_threshold: threshold_bytes(DEFAULT_YELLOW_PERCENT, memory_budget),
            yellow_exit_threshold: threshold_bytes(YELLOW_EXIT_PERCENT, memory_budget),
            red_threshold: threshold_bytes(DEFAULT_RED_PERCENT, memory_budget),
        }
    }

    /// Get the total number of lines across all tiers.
    #[must_use]
    #[inline]
    pub const fn line_count(&self) -> usize {
        self.line_count
    }

    /// Get the number of lines in hot tier.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub fn hot_line_count(&self) -> usize {
        self.hot.len()
    }

    /// Get the number of lines in warm tier.
    #[must_use]
    #[inline]
    pub const fn warm_line_count(&self) -> usize {
        self.warm.line_count()
    }

    /// Get the number of lines in cold tier.
    #[must_use]
    #[inline]
    pub const fn cold_line_count(&self) -> usize {
        self.cold.line_count()
    }

    /// Get the hot+warm memory usage (bytes).
    #[must_use]
    #[inline]
    pub const fn memory_used(&self) -> usize {
        self.hot
            .memory_used()
            .saturating_add(self.warm.memory_used())
    }

    /// Get the cold tier memory usage (bytes, compressed).
    #[must_use]
    #[inline]
    pub const fn cold_memory_used(&self) -> usize {
        self.cold.compressed_size()
    }

    /// Get total memory usage across all tiers (bytes), including cache and overhead.
    #[must_use]
    #[inline]
    pub const fn total_memory_used(&self) -> usize {
        self.bytes_used
    }

    /// Get reclaimable storage bytes used for budget enforcement.
    #[must_use]
    #[inline]
    pub const fn budgeted_memory_used(&self) -> usize {
        self.budgeted_bytes
    }

    /// Check if reclaimable storage exceeds the budget.
    #[must_use]
    #[inline]
    pub const fn over_budget(&self) -> bool {
        self.budgeted_bytes > self.memory_budget
    }

    /// Get the hot tier limit.
    #[must_use]
    #[inline]
    pub const fn hot_limit(&self) -> usize {
        self.hot_limit
    }

    /// Get the warm tier limit.
    #[must_use]
    #[inline]
    pub const fn warm_limit(&self) -> usize {
        self.warm_limit
    }

    /// Get the (clamped) block size.
    #[cfg(test)]
    #[must_use]
    #[inline]
    pub(crate) const fn block_size(&self) -> usize {
        self.block_size
    }

    /// Get the memory budget.
    #[must_use]
    #[inline]
    pub const fn memory_budget(&self) -> usize {
        self.memory_budget
    }

    /// Get the line limit (maximum total lines allowed).
    #[must_use]
    #[inline]
    pub const fn line_limit(&self) -> Option<usize> {
        self.line_limit
    }

    /// Get the current memory pressure watermark level (precomputed on every accounting sync).
    #[must_use]
    #[inline]
    pub const fn watermark_level(&self) -> WatermarkLevel {
        self.watermark_level
    }
}

impl Default for Scrollback {
    fn default() -> Self {
        Self::with_defaults()
    }
}

/// How many of `lines` carry an inline-image span.
pub(crate) fn count_image_rows(lines: &[Line]) -> u64 {
    let mut rows = 0u64;
    for line in lines {
        if line.has_images() {
            // discharges the strict L0 gate's overflow obligation.
            rows = rows.saturating_add(1);
        }
    }
    rows
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod decode_fuzz_tests;

#[cfg(test)]
#[path = "memory_tracking_tests.rs"]
mod memory_tracking_tests;

#[cfg(test)]
#[path = "storage_tests.rs"]
mod storage_tests;

#[cfg(test)]
#[path = "watermark_tests.rs"]
mod watermark_tests;
