// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates
use std::borrow::Cow;

use super::*;

/// Configuration for disk-backed scrollback.
///
/// # Examples
///
/// Create with builder pattern:
///
/// ```no_run
/// use aterm_scrollback::DiskBackedScrollbackConfig;
///
/// let config = DiskBackedScrollbackConfig::new("/tmp/session.dtrm")
///     .with_hot_limit(500)        // 500 lines in hot tier
///     .with_warm_limit(5_000)     // 5000 lines in warm tier
///     .with_block_size(256);      // lines per compressed block
/// ```
#[derive(Debug, Clone)]
pub struct DiskBackedScrollbackConfig {
    /// Hot tier limit (lines).
    pub hot_limit: usize,
    /// Warm tier limit (lines).
    pub warm_limit: usize,
    /// Memory budget (bytes).
    pub memory_budget: usize,
    /// Lines per compressed block.
    pub block_size: usize,
    /// Maximum total lines across all tiers (None = unlimited).
    ///
    /// Defaults to [`DEFAULT_LINE_LIMIT`], so runaway output cannot grow the
    /// scrollback (and the cold file) without bound;
    /// [`with_unlimited_lines`](Self::with_unlimited_lines) opts out.
    pub line_limit: Option<usize>,
    /// Disk cold tier configuration.
    pub cold_config: DiskColdConfig,
}

impl DiskBackedScrollbackConfig {
    /// Create a new config with the given cold storage path; the line limit
    /// defaults to [`DEFAULT_LINE_LIMIT`].
    #[must_use]
    pub fn new(cold_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            hot_limit: 1000,
            warm_limit: 10_000,
            memory_budget: 100_000_000,
            block_size: DEFAULT_BLOCK_SIZE,
            line_limit: Some(DEFAULT_LINE_LIMIT),
            cold_config: DiskColdConfig::new(cold_path),
        }
    }

    /// Remove the total-line limit (scrollback bounded only by memory budget
    /// and disk capacity).
    #[must_use]
    pub fn with_unlimited_lines(mut self) -> Self {
        self.line_limit = None;
        self
    }

    /// Set hot tier limit.
    #[must_use]
    pub fn with_hot_limit(mut self, limit: usize) -> Self {
        self.hot_limit = limit;
        self
    }

    /// Set warm tier limit.
    #[must_use]
    pub fn with_warm_limit(mut self, limit: usize) -> Self {
        self.warm_limit = limit;
        self
    }

    /// Set memory budget.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_memory_budget(mut self, budget: usize) -> Self {
        self.memory_budget = budget;
        self
    }

    /// Set block size.
    #[must_use]
    pub fn with_block_size(mut self, size: usize) -> Self {
        self.block_size = size;
        self
    }
}

/// Scrollback buffer with a disk-backed cold tier.
///
/// Like [`Scrollback`], but the cold tier lives in a memory-mapped file, so
/// long histories keep memory bounded.
///
/// # Examples
///
/// Create a disk-backed scrollback and push lines:
///
/// ```no_run
/// use aterm_scrollback::{DiskBackedScrollback, DiskBackedScrollbackConfig};
///
/// let config = DiskBackedScrollbackConfig::new("/tmp/session.dtrm")
///     .with_hot_limit(1000)
///     .with_warm_limit(10_000);
///
/// let mut sb = DiskBackedScrollback::with_config(config).unwrap();
/// sb.push_str("Hello, world!").unwrap();
/// sb.push_str("More history...").unwrap();
///
/// // Get the most recent line
/// let newest = sb.get_line(sb.line_count() - 1).unwrap().unwrap();
/// assert_eq!(newest.to_string(), "More history...");
///
/// // Check line count
/// assert_eq!(sb.line_count(), 2);
/// ```
#[derive(Debug)]
pub struct DiskBackedScrollback {
    /// Hot tier: uncompressed lines (instant access).
    hot: HotTier,
    /// Warm tier: LZ4 compressed blocks.
    warm: WarmTier,
    /// Cold tier: LZ4 compressed pages, disk-backed.
    cold: DiskColdTier,
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
    /// Running diagnostic total for hot + warm memory usage (includes cache + overhead).
    bytes_used: usize,
    /// Reclaimable hot + warm storage bytes for budget enforcement.
    budgeted_bytes: usize,
    /// Maximum total lines; older lines are discarded past it (None = no limit).
    line_limit: Option<usize>,
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

impl DiskBackedScrollback {
    /// Create a new disk-backed scrollback with the given configuration.
    #[must_use = "constructor returns a new DiskBackedScrollback that must be used"]
    pub fn with_config(config: DiskBackedScrollbackConfig) -> std::io::Result<Self> {
        let cold = DiskColdTier::with_config(config.cold_config)?;
        let cold_lines = cold.line_count();
        let hot = HotTier::new();
        let warm = WarmTier::new();
        let hot_limit = config.hot_limit.max(1);
        // Block size must not exceed the hot limit (else promotion never
        // triggers) nor MAX_DECODE_PAGE_LINES (else a legitimate block decodes
        // short and fails its line-count validation).
        let block_size = config
            .block_size
            .max(1)
            .min(hot_limit)
            .min(crate::line::MAX_DECODE_PAGE_LINES);

        let mut sb = Self {
            bytes_used: hot.memory_used() + warm.memory_used(),
            budgeted_bytes: 0,
            hot,
            warm,
            cold,
            hot_limit,
            warm_limit: config.warm_limit,
            memory_budget: config.memory_budget,
            block_size,
            line_count: cold_lines,
            line_limit: None,
            image_rows_dropped_by_compression: 0,
            watermark_level: WatermarkLevel::Green,
            yellow_threshold: threshold_bytes(DEFAULT_YELLOW_PERCENT, config.memory_budget),
            yellow_exit_threshold: threshold_bytes(YELLOW_EXIT_PERCENT, config.memory_budget),
            red_threshold: threshold_bytes(DEFAULT_RED_PERCENT, config.memory_budget),
        };
        // The setter truncates cold lines reloaded from disk to the cap.
        sb.set_line_limit(config.line_limit);
        Ok(sb)
    }

    /// Get the total number of lines across all tiers.
    #[must_use]
    #[inline]
    pub fn line_count(&self) -> usize {
        self.line_count
    }

    /// Get the number of lines in the hot tier.
    #[cfg(test)]
    #[must_use]
    #[inline]
    pub(crate) fn hot_line_count(&self) -> usize {
        self.hot.len()
    }

    /// Get the number of lines in the warm tier.
    #[cfg(test)]
    #[must_use]
    #[inline]
    pub(crate) fn warm_line_count(&self) -> usize {
        self.warm.line_count()
    }

    /// Get the number of lines in cold tier.
    #[must_use]
    #[inline]
    pub fn cold_line_count(&self) -> usize {
        self.cold.line_count()
    }

    /// Get the line limit (maximum total lines allowed).
    ///
    /// Returns `None` if no limit is set.
    #[must_use]
    #[inline]
    pub(crate) fn line_limit(&self) -> Option<usize> {
        self.line_limit
    }

    /// Monotonic count of history lines whose inline image was discarded by
    /// compression: the disk-backed twin of
    /// [`Scrollback::image_rows_dropped_by_compression`]. A picture never
    /// reaches the `.dtrm` file.
    ///
    /// [`Scrollback::image_rows_dropped_by_compression`]: crate::Scrollback::image_rows_dropped_by_compression
    #[must_use]
    #[inline]
    pub fn image_rows_dropped_by_compression(&self) -> u64 {
        self.image_rows_dropped_by_compression
    }

    /// Set the line limit (`None` removes it, `Some(0)` disables scrollback)
    /// and enforce it now. If truncation fails (decompression or I/O), the
    /// limit is set and enforcement waits for the next push.
    pub(crate) fn set_line_limit(&mut self, limit: Option<usize>) {
        self.line_limit = limit;
        if let Some(max) = limit
            && self.line_count > max
            && let Err(e) = self.truncate(max)
        {
            aterm_log::warn!(
                "set_line_limit: truncation to {max} failed ({e}), limit set but not yet enforced"
            );
        }
    }

    /// Push a new line to the scrollback.
    ///
    /// If a line limit is set and exceeded, the oldest line is discarded.
    pub(crate) fn push_line(&mut self, line: Line) -> std::io::Result<()> {
        if self.hot.len() >= self.hot_limit {
            self.promote_hot_to_warm()?;
        }

        self.hot.push(line);
        self.line_count += 1;
        self.sync_accounting();

        if let Some(max) = self.line_limit
            && self.line_count > max
            && let Err(e) = self.truncate(max)
        {
            aterm_log::warn!(
                "push_line: truncation to limit {max} failed ({e}), limit temporarily unenforced"
            );
        }

        // Eager promotion under memory pressure.
        if self.watermark_level >= WatermarkLevel::Yellow && self.hot.len() >= self.block_size {
            self.promote_hot_to_warm()?;
        }

        let _ = self.handle_memory_pressure();

        Ok(())
    }

    /// Push a line from a string.
    pub fn push_str(&mut self, s: &str) -> std::io::Result<()> {
        self.push_line(Line::from(s))
    }

    /// Get a line by index (0 = oldest).
    ///
    /// Returns `Cow::Borrowed` for hot-tier lines and `Cow::Owned` for warm and
    /// cold lines (decompressed on access); `Ok(None)` when out of bounds, `Err`
    /// on an I/O or decompression failure.
    #[must_use = "line data is discarded if not consumed"]
    pub fn get_line(&self, idx: usize) -> Result<Option<Cow<'_, Line>>, super::ScrollbackError> {
        if idx >= self.line_count {
            return Ok(None);
        }

        let cold_count = self.cold.line_count();
        let warm_count = self.warm.line_count();

        if idx < cold_count {
            // Line is in cold tier (decompressed → owned)
            self.cold.get_line(idx).map(|opt| opt.map(Cow::Owned))
        } else if idx < cold_count + warm_count {
            // Line is in warm tier (decompressed → owned)
            self.warm
                .get_line(idx - cold_count)
                .map(|opt| opt.map(Cow::Owned))
        } else {
            // Line is in hot tier (uncompressed → borrowed)
            let hot_idx = idx - cold_count - warm_count;
            let Some(line) = self.hot.get(hot_idx) else {
                return Err(super::ScrollbackError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("in-range line index {idx} mapped to missing hot tier index {hot_idx}"),
                )));
            };
            Ok(Some(Cow::Borrowed(line)))
        }
    }

    /// First logical index of the hot tier (`cold + warm` lines) — the dense
    /// walk's switch from bulk segment decodes to borrowed hot lines
    /// (`ScrollbackStorage::dense_from`).
    pub(crate) fn hot_start(&self) -> usize {
        crate::iter::hot_start_tiers(&self.cold, &self.warm)
    }

    /// Bulk read for the streaming iterators: owned lines from `idx` through
    /// the end of its tier segment (cold page, warm block or one hot line). The
    /// disk-cold path decodes a page once without caching it, so a full-history
    /// walk cannot evict the viewport's pages.
    // Skip: the tier-dispatch driver; each per-tier bulk read is classified on
    // its own.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn read_segment(&self, idx: usize) -> crate::iter::SegmentResult {
        crate::iter::read_segment_tiers(&self.cold, &self.warm, &self.hot, idx)
    }

    /// Get a line by reverse index (0 = newest).
    ///
    /// Returns `Ok(None)` for out-of-bounds, `Err` for I/O or decompression failures.
    pub(crate) fn get_line_rev(
        &self,
        rev_idx: usize,
    ) -> Result<Option<Cow<'_, Line>>, super::ScrollbackError> {
        if rev_idx >= self.line_count {
            return Ok(None);
        }
        self.get_line(self.line_count - 1 - rev_idx)
    }

    /// Snapshot of the hot and warm lines only, for checkpointing: skips the
    /// cold tier's disk I/O and decompression.
    #[must_use]
    pub fn checkpoint_snapshot_fast(&self) -> super::Scrollback {
        super::access::checkpoint_snapshot_fast_from(self)
    }

    /// Clear all lines.
    pub(crate) fn clear(&mut self) -> std::io::Result<()> {
        self.cold.clear()?; // fallible I/O first
        self.hot.clear();
        self.warm.clear();
        self.line_count = 0;
        self.sync_accounting();
        self.assert_bytes_used_invariant();
        Ok(())
    }

    /// Get the hot tier limit.
    #[must_use]
    #[inline]
    pub(crate) fn hot_limit(&self) -> usize {
        self.hot_limit
    }

    /// Get the warm tier limit.
    #[must_use]
    #[inline]
    pub(crate) fn warm_limit(&self) -> usize {
        self.warm_limit
    }

    /// Get the memory budget.
    #[must_use]
    #[inline]
    pub(crate) fn memory_budget(&self) -> usize {
        self.memory_budget
    }

    /// Set the memory budget (bytes).
    ///
    /// Enforces the new budget immediately by evicting warm blocks if needed.
    /// Returns `Err` if enforcement failed (I/O error or still over budget
    /// with unevictable warm blocks).
    pub(crate) fn set_memory_budget(&mut self, budget: usize) -> Result<(), ScrollbackError> {
        self.memory_budget = budget.max(1);
        self.yellow_threshold = threshold_bytes(DEFAULT_YELLOW_PERCENT, self.memory_budget);
        self.yellow_exit_threshold = threshold_bytes(YELLOW_EXIT_PERCENT, self.memory_budget);
        self.red_threshold = threshold_bytes(DEFAULT_RED_PERCENT, self.memory_budget);
        // Reset watermark before recomputing: threshold changes are a fresh
        // assessment, not subject to hysteresis from the old configuration.
        self.watermark_level = WatermarkLevel::Green;
        self.update_watermark_level();
        let _ = self.handle_memory_pressure();
        self.assert_bytes_used_invariant();
        if self.over_budget() && self.warm.block_count() > 0 {
            let over_bytes = self.budgeted_bytes.saturating_sub(self.memory_budget);
            return Err(ScrollbackError::EnforcementFailed { over_bytes });
        }
        Ok(())
    }

    /// Update both diagnostic and budget aggregates from per-tier counters. O(1).
    fn sync_accounting(&mut self) {
        self.bytes_used = self.hot.memory_used() + self.warm.memory_used();
        self.budgeted_bytes = self.hot.budgeted_bytes() + self.warm.budgeted_bytes();
        self.update_watermark_level();
    }

    #[cfg(any(test, debug_assertions))]
    fn recompute_hot_warm_memory_used(&self) -> usize {
        self.hot.recompute_memory_used() + self.warm.recompute_memory_used()
    }

    #[cfg(any(test, debug_assertions))]
    fn recompute_budgeted_bytes(&self) -> usize {
        self.hot.recompute_budgeted_bytes() + self.warm.recompute_budgeted_bytes()
    }

    #[cfg(any(test, debug_assertions))]
    pub(crate) fn recompute_total_memory_used(&self) -> usize {
        self.recompute_hot_warm_memory_used() + self.cold.recompute_memory_used()
    }

    fn assert_bytes_used_invariant(&self) {
        #[cfg(any(test, debug_assertions))]
        {
            debug_assert_eq!(
                self.bytes_used,
                self.recompute_hot_warm_memory_used(),
                "disk scrollback hot/warm bytes_used counter drift",
            );
            debug_assert_eq!(
                self.budgeted_bytes,
                self.recompute_budgeted_bytes(),
                "disk scrollback budgeted_bytes counter drift",
            );
            debug_assert_eq!(
                self.total_memory_used(),
                self.recompute_total_memory_used(),
                "disk scrollback total memory counter drift",
            );
        }
    }
}

#[path = "disk_backed_truncate.rs"]
mod truncate;

#[path = "disk_backed_memory.rs"]
mod memory;

#[path = "disk_backed_tiers.rs"]
mod tiers;
