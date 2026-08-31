// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// Trust: opt into the `trust` tool-attribute namespace ONLY under a trustc
// verification build (`cfg(trust_verify)`, injected by `trustc -Z trust-verify`).
// A normal stable build sees neither the feature gate nor the attributes, so this
// is a no-op there; under Trust it enables `#[trust::backing]` / `#[trust::single_writer]`
// on the mmap types so the compiler PROVES their HIGH-2 spatial bounds and
// temporal (truncation) safety.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]
#![cfg_attr(not(trust_verify), allow(unexpected_cfgs))]
#![deny(unsafe_op_in_unsafe_fn)]
// F11-4 (#7941): production unwrap() is forbidden; tests opt out uniformly at the
// crate root so a missing per-module allow can't silently slip past clippy.
#![deny(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! Tiered scrollback storage for terminal emulators.
//!
//! Extracted from `aterm-core` as part of the monolith split (#2341).
//!
//! ## Design
//!
//! Three-tier architecture for memory efficiency:
//!
//! - **Hot tier**: Recent lines in RAM (uncompressed, instant access)
//! - **Warm tier**: Older lines in RAM (LZ4 compressed, ~10x compression)
//! - **Cold tier**: Oldest lines (Zstd compressed, in-memory or disk-backed)
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
//! │  │ Scrollback: in-memory  /  DiskBacked: mmap          │   │
//! │  └─────────────────────────────────────────────────────┘   │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Implementations
//!
//! - [`Scrollback`]: In-memory cold tier with budget-aware cold eviction.
//!   Memory budget covers hot + warm + cold compressed pages.
//! - [`DiskBackedScrollback`]: Disk-backed cold tier via memory-mapped file.
//!   Enables unlimited history with disk offloading; hot+warm RAM is bounded,
//!   while cold tier keeps an in-memory index + cache for fast lookups.
//!
//! ## Memory Targets (Total, All Tiers)
//!
//! These are typical total memory usage figures including cold tier.
//! For [`DiskBackedScrollback`], hot+warm RAM usage is bounded by budget; cold tier
//! stores bulk data on disk while keeping a small in-memory index + cache.
//!
//! | Lines | Typical Memory |
//! |-------|----------------|
//! | 100K  | ~2 MB          |
//! | 1M    | ~20 MB         |
//! | 10M   | ~200 MB        |
//!
//! ## Verification
//!
//! - TLA+ spec: `tla/Scrollback.tla`
//! - Kani proofs: `tier_transition_preserves_count`, `line_limit_enforced`
//! - Property tests: line count always accurate, no data loss

#![deny(missing_docs)]
#![deny(clippy::all)]

mod access;
mod codec;
mod cold_tier;
// Disk-backed cold tier (mmap/libc/std::fs). Opt-in per §2.7; default build is
// a headless hot(RAM)+warm(LZ4) scrollback with no platform dependencies.
#[cfg(feature = "disk-tier")]
mod disk;
#[cfg(feature = "disk-tier")]
mod disk_backed;
#[cfg(feature = "disk-tier")]
mod disk_format;
mod error;
mod hot_tier;
mod iter;
mod line;
mod log_shim;
#[cfg(any(fuzzing, feature = "fuzz"))]
pub mod lz4;
#[cfg(not(any(fuzzing, feature = "fuzz")))]
pub(crate) mod lz4;
// LZ4 decompression safety proofs (#7934). #[cfg(kani)] gates the proofs;
// unit tests remain visible under `cargo test` for non-Kani coverage.
#[cfg(any(kani, test))]
mod lz4_kani;
// Thin libc mmap wrapper — only compiled for the disk-backed cold tier.
#[cfg(feature = "disk-tier")]
pub mod mmap;
mod scrollback_accounting;
mod search_content;
mod storage;
mod tier;
mod tier_capabilities;
mod tier_ops;
mod watermark;

pub use access::ScrollbackAccess;
pub use aterm_rle::Rle;
pub use codec::DEFAULT_LINE_LIMIT;
pub(crate) use codec::{
    DEFAULT_BLOCK_SIZE, DEFAULT_HOT_LIMIT, DEFAULT_MEMORY_BUDGET, DEFAULT_WARM_LIMIT,
    binary_search_counted, decode_cold_bounded, decompress_lz4_bounded, encode_cold_block,
};
// zstd codec helper. Referenced by the on-disk `.dtrm` cold format
// (disk_compaction.rs / disk_memory.rs, behind `disk-tier`) and by the zstd
// round-trip unit test. With `zstd` but not `disk-tier` it has no production
// caller, so suppress the unused-import lint in that configuration.
#[cfg(feature = "zstd")]
#[cfg_attr(not(feature = "disk-tier"), allow(unused_imports))]
pub(crate) use codec::decode_zstd_bounded;
// Single source of truth for the cold-tier zstd level, shared by the in-memory
// cold tier (codec.rs) and the disk `.dtrm` rewrite/compaction paths so all
// eviction sites compress at the same CPU/ratio tradeoff.
#[cfg(feature = "zstd")]
#[cfg_attr(not(feature = "disk-tier"), allow(unused_imports))]
pub(crate) use codec::COLD_ZSTD_LEVEL;
pub(crate) use cold_tier::ColdTier;
#[cfg(feature = "disk-tier")]
pub(crate) use disk::DiskColdTier;
#[cfg(feature = "disk-tier")]
pub use disk_backed::{DiskBackedScrollback, DiskBackedScrollbackConfig};
#[cfg(feature = "disk-tier")]
pub(crate) use disk_format::DiskColdConfig;
pub use error::ScrollbackError;
pub(crate) use hot_tier::HotTier;
pub use iter::{ScrollbackIter, ScrollbackRevIter};
pub use line::{AttrRunCursor, CellAttrs, HyperlinkSpan, ImageSpan, Line, UnderlineColorSpan};
// Block codec, public for `TerminalCheckpoint` grid-body encode/decode (B.3.2).
pub use line::{
    deserialize_lines, deserialize_lines_strict, deserialize_lines_tail_strict, serialize_lines,
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
/// Lines flow from hot → warm → cold as they age.
/// Memory budget is enforced by evicting warm blocks to cold tier, then
/// dropping oldest cold pages if total memory is still over budget.
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
///
/// // Iterate newest to oldest
/// let recent: Vec<_> = sb.iter_rev().take(2).map(|l| l.to_string()).collect();
/// assert_eq!(recent, vec!["Line C", "Line B"]);
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
    /// Cold tier: Zstd compressed, in-memory.
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
    /// Maximum total lines allowed (None = no limit).
    /// When set, older lines are discarded when this limit is exceeded.
    line_limit: Option<usize>,
    /// Monotonic count of lines DROPPED by memory-pressure eviction (cold-tier
    /// FIFO under the byte budget) — real retention loss, exposed out-of-band
    /// (audit E10a). User-requested `set_line_limit` truncation is NOT counted.
    pressure_evicted_lines: u64,
    /// Monotonic count of history lines that crossed the IMAGE RETENTION
    /// HORIZON — see
    /// [`image_rows_dropped_by_compression`](Self::image_rows_dropped_by_compression).
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
    /// Create a new scrollback buffer with specified tier limits.
    ///
    /// # Arguments
    /// * `hot_limit` - Maximum lines in hot tier before promotion
    /// * `warm_limit` - Maximum lines in warm tier before eviction
    /// * `memory_budget` - Total memory budget in bytes
    ///
    /// The line limit defaults to [`DEFAULT_LINE_LIMIT`] (100,000 lines)
    /// to prevent unbounded scrollback growth from runaway stdout (#7929).
    /// Callers that want unlimited history should call
    /// [`set_line_limit(None)`](Self::set_line_limit) after construction.
    ///
    /// ENSURES: self.line_count() == 0
    /// ENSURES: self.hot_limit() >= 1 (clamped from input)
    /// ENSURES: self.memory_used() == 0
    /// ENSURES: self.line_limit() == Some(DEFAULT_LINE_LIMIT)
    #[must_use]
    pub fn new(hot_limit: usize, warm_limit: usize, memory_budget: usize) -> Self {
        let hot = HotTier::new();
        let warm = WarmTier::new();
        let cold = ColdTier::new();
        Self {
            // Saturating: fresh-tier struct sizes, so the sum always fits in
            // `usize` — the saturation just discharges the strict L0 gate's
            // unconstrained-input overflow counterexample.
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

    /// Create a scrollback buffer with sensible defaults.
    ///
    /// Uses: 1000 hot lines, 10000 warm lines, 100MB budget.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_HOT_LIMIT, DEFAULT_WARM_LIMIT, DEFAULT_MEMORY_BUDGET)
    }

    /// Create a scrollback buffer with custom block size.
    ///
    /// ENSURES: self.line_count() == 0
    /// ENSURES: self.hot_limit() >= 1
    /// ENSURES: block_size >= 1 && block_size <= hot_limit
    #[must_use]
    pub fn with_block_size(
        hot_limit: usize,
        warm_limit: usize,
        memory_budget: usize,
        block_size: usize,
    ) -> Self {
        let hot_limit = hot_limit.max(1);
        // Block size must not exceed hot limit, otherwise promotion never triggers.
        // Also clamp to MAX_DECODE_PAGE_LINES so a serialized block never holds more
        // lines than the disk-decode cap will reconstruct — otherwise a legitimate
        // oversized block would decode short and fail its line-count validation.
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
    pub fn line_count(&self) -> usize {
        self.line_count
    }

    /// Get the number of lines in hot tier.
    #[must_use]
    #[inline]
    pub fn hot_line_count(&self) -> usize {
        self.hot.len()
    }

    /// Get the number of lines in warm tier.
    #[must_use]
    #[inline]
    pub fn warm_line_count(&self) -> usize {
        self.warm.line_count()
    }

    /// Get the number of lines in cold tier.
    #[must_use]
    #[inline]
    pub fn cold_line_count(&self) -> usize {
        self.cold.line_count()
    }

    /// Get the hot+warm memory usage (bytes).
    ///
    /// Use [`cold_memory_used`](Self::cold_memory_used) to inspect the cold tier
    /// separately. [`total_memory_used`](Self::total_memory_used) includes all tiers.
    #[must_use]
    pub fn memory_used(&self) -> usize {
        // Saturating: both tiers report bytes of memory this process actually
        // holds, so the true sum always fits in `usize` (see `Scrollback::new`).
        self.hot
            .memory_used()
            .saturating_add(self.warm.memory_used())
    }

    /// Get the cold tier memory usage (bytes, compressed).
    ///
    /// For [`Scrollback`], cold tier is in-memory and participates in budget
    /// enforcement via FIFO eviction of the oldest cold pages.
    /// For disk-backed cold storage, use `DiskBackedScrollback::cold_memory_used`
    /// for cache/metadata memory and `DiskBackedScrollback::cold_disk_used` for disk usage.
    #[must_use]
    pub fn cold_memory_used(&self) -> usize {
        self.cold.compressed_size()
    }

    /// Get total memory usage across all tiers (bytes).
    ///
    /// Returns hot + warm + cold tier memory.
    #[must_use]
    pub fn total_memory_used(&self) -> usize {
        self.bytes_used
    }

    /// Get reclaimable storage bytes used for budget enforcement.
    ///
    /// Unlike [`total_memory_used`](Self::total_memory_used), this excludes
    /// diagnostic cache and metadata growth. This is the quantity compared
    /// against [`memory_budget`](Self::memory_budget) by
    /// [`over_budget`](Self::over_budget) and [`watermark_level`](Self::watermark_level).
    #[must_use]
    #[inline]
    pub fn budgeted_memory_used(&self) -> usize {
        self.budgeted_bytes
    }

    /// Check if reclaimable storage exceeds the budget.
    ///
    /// Uses `budgeted_bytes` (reclaimable storage only), not `total_memory_used()`
    /// (diagnostic, includes cache and metadata). This prevents read-only cache
    /// fills from perturbing the budget signal.
    #[must_use]
    #[inline]
    pub fn over_budget(&self) -> bool {
        self.budgeted_bytes > self.memory_budget
    }

    /// Get the hot tier limit.
    #[must_use]
    #[inline]
    pub fn hot_limit(&self) -> usize {
        self.hot_limit
    }

    /// Get the warm tier limit.
    #[must_use]
    #[inline]
    pub fn warm_limit(&self) -> usize {
        self.warm_limit
    }

    /// Get the (clamped) block size — lines per promoted warm block.
    ///
    /// Always `<= MAX_DECODE_PAGE_LINES`, so a serialized block never exceeds the
    /// disk-decode line-count cap.
    #[cfg(test)]
    #[must_use]
    #[inline]
    pub(crate) fn block_size(&self) -> usize {
        self.block_size
    }

    /// Get the memory budget.
    #[must_use]
    #[inline]
    pub fn memory_budget(&self) -> usize {
        self.memory_budget
    }

    /// Get the line limit (maximum total lines allowed).
    ///
    /// Returns `None` if no limit is set.
    #[must_use]
    #[inline]
    pub fn line_limit(&self) -> Option<usize> {
        self.line_limit
    }

    /// Get the current memory pressure watermark level.
    ///
    /// This is a lightweight O(1) read — the level is precomputed on every
    /// accounting sync, not recalculated on each call.
    #[must_use]
    #[inline]
    pub fn watermark_level(&self) -> WatermarkLevel {
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
/// The ONE seam both tiered stores count the image retention horizon through
/// ([`Scrollback`] and `DiskBackedScrollback` each compress hot lines in their
/// own `promote_hot_to_warm`), so the two counters cannot come to mean
/// different things. Counts LINES, not spans: a row covered by two placements
/// side by side is still one row of history that lost its pictures, and the
/// sibling loss counters ([`Scrollback::pressure_evicted_lines`]) are line
/// counts too.
///
/// Costs one `Option` discriminant test per promoted line — `Line::images` is
/// `None` for every line of every session that never drew a picture.
pub(crate) fn count_image_rows(lines: &[Line]) -> u64 {
    let mut rows = 0u64;
    for line in lines {
        if line.has_images() {
            // Saturating: `lines` is one warm block (thousands at most), so the
            // count cannot approach u64::MAX on any real path — the saturation
            // only discharges the strict L0 gate's unconstrained-input overflow
            // obligation.
            rows = rows.saturating_add(1);
        }
    }
    rows
}

// Tests for the top-level Scrollback type and integration tests.
#[cfg(test)]
mod tests;

#[cfg(test)]
mod mem_measure_tests;

// Tests for disk-backed scrollback.
#[cfg(all(test, feature = "disk-tier"))]
#[path = "disk_backed_tests.rs"]
mod disk_backed_tests;

// Regression tests for #5928: cold tier reload data integrity.
#[cfg(all(test, feature = "disk-tier"))]
#[path = "disk_backed_reload_tests.rs"]
mod disk_backed_reload_tests;

// Incremental memory-tracking regression tests.
#[cfg(test)]
#[path = "memory_tracking_tests.rs"]
mod memory_tracking_tests;

// Tests for the ScrollbackStorage abstraction.
#[cfg(test)]
#[path = "storage_tests.rs"]
mod storage_tests;

// Watermark backpressure system tests (#5233).
#[cfg(test)]
#[path = "watermark_tests.rs"]
mod watermark_tests;

// Kani proofs for tier-level invariants.
#[cfg(kani)]
mod kani_proofs;

// TRUST_NATIVE_TLA §4 (Phase 4): the kani-half of the unified verifier ledger.
//
// CRITICAL — this module is UN-GATED w.r.t. `cfg(kani)` (it is NOT under the
// `#[cfg(kani)]` on `kani_proofs`). The `#[kani::proof]` harnesses are dormant under
// stock cargo; the `proof_anchor!` REGISTRATIONS must NOT share that gate or they would
// never register in normal/test builds. They are decoupled — they name the harness by
// string and are gated ONLY by the `spec-anchors` feature (which the `spec_xref_closure`
// gate turns on for collection; production leaves it off so aterm-spec is not linked).
#[cfg(any(test, feature = "spec-anchors"))]
mod spec_proof_anchors;
