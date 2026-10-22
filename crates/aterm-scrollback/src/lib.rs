// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// Trust: opt into the `trust` tool-attribute namespace ONLY under a trustc
// verification build (`cfg(trust_verify)`, injected by `trustc -Z trust-verify`).
// A normal stable build sees neither the feature gate nor the attributes, so this
// is a no-op there; under Trust it enables the `#[trust::...]` attributes
// (`trust::skip` on the paths the verifier does not model).
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]
// F11-4 (#7941): production unwrap() is forbidden; tests opt out uniformly at the
// crate root so a missing per-module allow can't silently slip past clippy.
#![warn(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! Tiered scrollback storage for terminal emulators.
//!
//! ## Design
//!
//! Three-tier architecture for memory efficiency:
//!
//! - **Hot tier**: Recent lines in RAM (uncompressed, instant access)
//! - **Warm tier**: Older lines in RAM (LZ4 compressed, ~10x compression)
//! - **Cold tier**: Oldest lines (LZ4 compressed, in memory)
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │  HOT TIER (RAM) - Last ~1000 lines                          │
//! │  ┌─────────────────────────────────────────────────────┐   │
//! │  │ Uncompressed, instant access, ~200KB                │   │
//! │  └─────────────────────────────────────────────────────┘   │
//! │                         ↓ Age out                          │
//! │  WARM TIER (RAM, Compressed) - Last ~10K lines              │
//! │  ┌─────────────────────────────────────────────────────┐   │
//! │  │ LZ4 compressed blocks, ~50KB (10x compression)      │   │
//! │  └─────────────────────────────────────────────────────┘   │
//! │                         ↓ Age out                          │
//! │  COLD TIER - Oldest history                                 │
//! │  ┌─────────────────────────────────────────────────────┐   │
//! │  │ LZ4 compressed pages, in memory                     │   │
//! │  └─────────────────────────────────────────────────────┘   │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Implementations
//!
//! - [`Scrollback`]: In-memory cold tier with budget-aware cold eviction.
//!   Memory budget covers hot + warm + cold compressed pages.
//!
//! ## Memory Targets (Total, All Tiers)
//!
//! These are typical total memory usage figures including cold tier.
//!
//! | Lines | Typical Memory |
//! |-------|----------------|
//! | 100K  | ~2 MB          |
//! | 1M    | ~20 MB         |
//! | 10M   | ~200 MB        |
//!
//! ## Under a Binding Budget
//!
//! The budget counts stored bytes: the hot tier raw, the warm and cold tiers
//! compressed. When it binds ([`Scrollback::set_memory_budget`], aterm-gui's
//! memory-pressure shed), the store keeps the newest lines that fit, so the
//! codec's ratio sets how much history survives. Against zstd, LZ4 keeps
//! 19-37% fewer lines under a binding budget and stores 20-35% more bytes
//! under one that does not (docs/AUDIT-third-party-2026-10-15.md, ZC.2).
//!
//! - The tier laws are the `ty` models `EvictFull` and `TierResidency`
//!   (aterm-spec), bound by aterm-buffer's conformance suites; the tiers
//!   themselves by `src/tests/` and `cold_tier_tests.rs`.
//! - Exhaustive sweeps: the watermark thresholds over every budget up to
//!   100,000 (`thresholds_are_bounded_for_every_budget`), the LZ4 decoder's
//!   claim law over every small input (`lz4::tests`).

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
// Block codec, public for `TerminalCheckpoint` grid-body encode/decode.
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
///
/// Lines flow hot → warm → cold as they age. The memory budget is enforced by
/// evicting warm blocks to cold, then dropping the oldest cold pages.
///
/// # Examples
///
/// Basic usage with string lines:
///
/// ```
/// use aterm_scrollback::Scrollback;
///
/// let mut sb = Scrollback::with_defaults();
/// sb.push_str("First line");
/// sb.push_str("Second line");
///
/// // Get lines by index (0 = oldest)
/// assert_eq!(sb.get_line(0).expect("ok").expect("some").to_string(), "First line");
/// assert_eq!(sb.get_line(1).expect("ok").expect("some").to_string(), "Second line");
///
/// // Get lines by reverse index (0 = newest)
/// assert_eq!(sb.get_line_rev(0).expect("ok").expect("some").to_string(), "Second line");
/// ```
///
/// Iterating over scrollback history:
///
/// ```
/// use aterm_scrollback::Scrollback;
///
/// let mut sb = Scrollback::with_defaults();
/// sb.push_str("Line A");
/// sb.push_str("Line B");
/// sb.push_str("Line C");
///
/// // Iterate oldest to newest
/// let lines: Vec<_> = sb.iter().map(|l| l.to_string()).collect();
/// assert_eq!(lines, vec!["Line A", "Line B", "Line C"]);
/// ```
///
/// Setting a line limit:
///
/// ```
/// use aterm_scrollback::Scrollback;
///
/// let mut sb = Scrollback::with_defaults();
/// sb.set_line_limit(Some(3));
///
/// sb.push_str("One");
/// sb.push_str("Two");
/// sb.push_str("Three");
/// sb.push_str("Four");  // "One" gets discarded
///
/// assert_eq!(sb.line_count(), 3);
/// assert_eq!(sb.get_line(0).expect("ok").expect("some").to_string(), "Two");
/// ```
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
    /// `set_line_limit` truncation is not counted.
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
    /// Create a new scrollback buffer with the given hot and warm line limits
    /// and memory budget (bytes); `hot_limit` is clamped to at least 1.
    ///
    /// The line limit defaults to [`DEFAULT_LINE_LIMIT`] against runaway
    /// output; call [`set_line_limit(None)`](Self::set_line_limit) for
    /// unlimited history.
    #[must_use]
    pub fn new(hot_limit: usize, warm_limit: usize, memory_budget: usize) -> Self {
        let hot = HotTier::new();
        let warm = WarmTier::new();
        let cold = ColdTier::new();
        Self {
            // Saturating: fresh-tier sizes always fit; this only discharges the
            // strict L0 gate's unconstrained-input overflow counterexample.
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

    /// Create a scrollback buffer with the defaults: 1000 hot lines, 10000 warm
    /// lines, a 100 MB budget.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_HOT_LIMIT, DEFAULT_WARM_LIMIT, DEFAULT_MEMORY_BUDGET)
    }

    /// Create a scrollback buffer with a custom block size, clamped to
    /// `1..=min(hot_limit, MAX_DECODE_PAGE_LINES)`.
    #[must_use]
    pub fn with_block_size(
        hot_limit: usize,
        warm_limit: usize,
        memory_budget: usize,
        block_size: usize,
    ) -> Self {
        let hot_limit = hot_limit.max(1);
        // Not above hot limit (else promotion never triggers), nor above
        // MAX_DECODE_PAGE_LINES (else a legitimate block decodes short and
        // fails its line-count validation).
        let block_size = block_size
            .max(1)
            .min(hot_limit)
            .min(crate::line::MAX_DECODE_PAGE_LINES);
        let hot = HotTier::new();
        let warm = WarmTier::new();
        let cold = ColdTier::new();
        Self {
            // Saturating: fresh-tier struct sizes (see `Scrollback::new`).
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

    /// Get the hot+warm memory usage (bytes); see
    /// [`cold_memory_used`](Self::cold_memory_used) and
    /// [`total_memory_used`](Self::total_memory_used).
    #[must_use]
    #[inline]
    pub const fn memory_used(&self) -> usize {
        // Saturating: both are bytes this process holds, so the sum fits.
        self.hot
            .memory_used()
            .saturating_add(self.warm.memory_used())
    }

    /// Get the cold tier memory usage (bytes, compressed).
    ///
    /// For [`Scrollback`], cold tier is in-memory and participates in budget
    /// enforcement via FIFO eviction of the oldest cold pages.
    #[must_use]
    #[inline]
    pub const fn cold_memory_used(&self) -> usize {
        self.cold.compressed_size()
    }

    /// Get total memory usage across all tiers (bytes), including cache and
    /// overhead.
    #[must_use]
    #[inline]
    pub const fn total_memory_used(&self) -> usize {
        self.bytes_used
    }

    /// Get reclaimable storage bytes used for budget enforcement: unlike
    /// [`total_memory_used`](Self::total_memory_used), excludes cache and
    /// metadata. [`over_budget`](Self::over_budget) and the watermark compare it.
    #[must_use]
    #[inline]
    pub const fn budgeted_memory_used(&self) -> usize {
        self.budgeted_bytes
    }

    /// Check if reclaimable storage exceeds the budget. Uses `budgeted_bytes`,
    /// not `total_memory_used()`, so read-only cache fills do not perturb it.
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

    /// Get the (clamped) block size: lines per promoted warm block, always
    /// `<= MAX_DECODE_PAGE_LINES`.
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
    ///
    /// Returns `None` if no limit is set.
    #[must_use]
    #[inline]
    pub const fn line_limit(&self) -> Option<usize> {
        self.line_limit
    }

    /// Get the current memory pressure watermark level (precomputed on every
    /// accounting sync).
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

/// How many of `lines` carry an inline-image span — i.e. how many FOOTPRINT
/// ROWS lose their picture when this batch is compressed.
///
/// The one seam the tiered store counts the image retention horizon through
/// (from [`Scrollback`]'s `promote_hot_to_warm`). Counts LINES, not spans: a row covered by two placements
/// side by side is still one row of history that lost its pictures, and the
/// sibling loss counters ([`Scrollback::pressure_evicted_lines`]) are line
/// counts too.
///
/// The one seam both [`Scrollback`] and `DiskBackedScrollback` count the image
/// retention horizon through, so their counters mean the same thing. Counts
/// lines, not spans, like the sibling loss counters.
pub(crate) fn count_image_rows(lines: &[Line]) -> u64 {
    let mut rows = 0u64;
    for line in lines {
        if line.has_images() {
            // Saturating: one warm block cannot approach u64::MAX; this only
            // discharges the strict L0 gate's overflow obligation.
            rows = rows.saturating_add(1);
        }
    }
    rows
}

#[cfg(test)]
mod tests;

// Never-panic fuzz of the decode surfaces; runs in every lib test build.
#[cfg(test)]
mod decode_fuzz_tests;

// Incremental memory-tracking regression tests.
#[cfg(test)]
#[path = "memory_tracking_tests.rs"]
mod memory_tracking_tests;

#[cfg(test)]
#[path = "storage_tests.rs"]
mod storage_tests;

#[cfg(test)]
#[path = "watermark_tests.rs"]
mod watermark_tests;
