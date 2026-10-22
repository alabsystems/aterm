// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::borrow::Cow;

use super::access::ScrollbackAccess;
use super::{Line, Scrollback, ScrollbackError, WatermarkLevel};

/// Iterator over scrollback storage lines (oldest to newest).
///
/// Streams whole decoded segments: each warm block or cold page is decoded once
/// and its lines moved out. A corrupt segment is skipped whole (one warning per
/// segment), matching [`super::ScrollbackIter`].
pub struct ScrollbackStorageIter<'a> {
    pub(crate) storage: &'a ScrollbackStorage,
    pub(crate) idx: usize,
    /// Decoded lines of the current segment, drained front-to-back.
    pub(crate) buf: std::collections::VecDeque<Line>,
}

impl Iterator for ScrollbackStorageIter<'_> {
    type Item = Line;

    // Skip: the segment-walk driver — routes into the per-tier bulk reads
    // (each individually classified: guarded-index / decode class).
    #[cfg_attr(trust_verify, trust::skip)]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(line) = self.buf.pop_front() {
                // saturating_add discharges the strict L0 gate's unconstrained-`idx`
                // overflow counterexample; `idx < total` on every real path.
                self.idx = self.idx.saturating_add(1);
                return Some(line);
            }
            let total = self.storage.line_count();
            if self.idx >= total {
                return None;
            }
            match self.storage.read_segment(self.idx) {
                Ok(lines) => {
                    if lines.is_empty() {
                        // In-range index with no data (stale count or short
                        // decode): end of data.
                        return None;
                    }
                    self.buf = std::collections::VecDeque::from(lines);
                }
                Err((e, skip)) => {
                    // Direct `warn!`: an error cannot go through a log shim (see
                    // log_shim.rs), which leaves one documented full-verify gap.
                    aterm_log::warn!("storage iter: skipping {skip} line(s) at {}: {e}", self.idx);
                    // Clamp so a corrupt tail segment cannot push the cursor
                    // past `total`; `max(1)` guarantees forward progress.
                    let skip = skip.min(total.saturating_sub(self.idx)).max(1);
                    self.idx = self.idx.saturating_add(skip);
                }
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.storage.line_count().saturating_sub(self.idx);
        (0, Some(remaining))
    }
}

/// Unified scrollback storage abstraction.
///
/// The one backend is the in-memory tiered [`Scrollback`]; `Grid` holds it
/// through this enum.
///
/// # Examples
///
/// Using with memory storage (default):
///
/// ```
/// use aterm_scrollback::{ScrollbackStorage, Scrollback, Line};
///
/// // Create from Scrollback
/// let mut storage: ScrollbackStorage = Scrollback::with_defaults().into();
///
/// // Push lines (returns Result: the trait's I/O-shaped signature)
/// storage.push_line(Line::from("Hello")).unwrap();
/// storage.push_line(Line::from("World")).unwrap();
///
/// // Access lines
/// assert_eq!(storage.line_count(), 2);
/// assert_eq!(storage.get_line_rev(0).unwrap().unwrap().to_string(), "World");
/// ```
#[non_exhaustive]
#[derive(Debug)]
pub enum ScrollbackStorage {
    /// In-memory tiered scrollback.
    Memory(Scrollback),
}

// Static dispatch over the concrete backend: through `&dyn ScrollbackAccess`
// the strict L0 gate cannot resolve the callee in the lowered bundle and
// charges every delegation a "may panic" obligation. Matching on the enum calls
// the same `ScrollbackAccess` impl the vtable would.
macro_rules! dispatch {
    ($self:expr, $method:ident($($arg:expr),*)) => {
        match $self {
            ScrollbackStorage::Memory(sb) => ScrollbackAccess::$method(sb $(, $arg)*),
        }
    };
}

impl ScrollbackStorage {
    // --- Delegated via ScrollbackAccess ---

    /// Get the total number of lines.
    #[must_use]
    #[inline]
    pub fn line_count(&self) -> usize {
        dispatch!(self, line_count())
    }

    /// Get a line by index (0 = oldest).
    ///
    /// Returns `Cow::Borrowed` for hot-tier lines (zero-copy) and
    /// `Cow::Owned` for warm/cold-tier lines (decompressed on access).
    ///
    /// Returns `Ok(None)` for out-of-bounds, `Err` for I/O or decompression failures.
    #[must_use = "line data is discarded if not consumed"]
    #[inline]
    pub fn get_line(&self, idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError> {
        dispatch!(self, get_line(idx))
    }

    /// Get a line by reverse index (0 = newest).
    ///
    /// Returns `Ok(None)` for out-of-bounds, `Err` for I/O or decompression failures.
    #[must_use = "line data is discarded if not consumed"]
    #[inline]
    pub fn get_line_rev(&self, rev_idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError> {
        dispatch!(self, get_line_rev(rev_idx))
    }

    /// Get the hot tier limit.
    #[must_use]
    #[inline]
    pub fn hot_limit(&self) -> usize {
        dispatch!(self, hot_limit())
    }

    /// Get the warm tier limit.
    #[must_use]
    #[inline]
    pub fn warm_limit(&self) -> usize {
        dispatch!(self, warm_limit())
    }

    /// Get the line limit.
    #[must_use]
    #[inline]
    pub fn line_limit(&self) -> Option<usize> {
        dispatch!(self, line_limit())
    }

    /// Get the memory budget.
    #[must_use]
    #[inline]
    pub fn memory_budget(&self) -> usize {
        dispatch!(self, memory_budget())
    }

    /// Get reclaimable storage bytes used for budget enforcement.
    #[must_use]
    #[inline]
    pub fn budgeted_memory_used(&self) -> usize {
        dispatch!(self, budgeted_memory_used())
    }

    /// Get total memory usage across all tiers (bytes).
    #[must_use]
    #[inline]
    pub fn total_memory_used(&self) -> usize {
        dispatch!(self, total_memory_used())
    }

    /// Get the number of lines in cold tier.
    #[must_use]
    #[inline]
    pub fn cold_line_count(&self) -> usize {
        dispatch!(self, cold_line_count())
    }

    /// Get cold tier memory usage (bytes).
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub fn cold_memory_used(&self) -> usize {
        dispatch!(self, cold_memory_used())
    }

    /// Get the current memory pressure watermark level.
    ///
    /// Lightweight O(1) read — the level is precomputed on every accounting sync.
    #[must_use]
    #[inline]
    pub fn watermark_level(&self) -> WatermarkLevel {
        dispatch!(self, watermark_level())
    }

    /// Lines dropped by memory-pressure eviction: real retention loss, reported
    /// out of band. It also counts the lines of a quarantined warm block, whose
    /// rows stay in
    /// [`line_count`](Self::line_count) as blank rows when older history sits
    /// before them, so the two can overlap.
    #[must_use]
    #[inline]
    pub const fn pressure_evicted_lines(&self) -> u64 {
        match self {
            ScrollbackStorage::Memory(sb) => sb.pressure_evicted_lines(),
        }
    }

    /// History lines whose inline image was discarded because the line was
    /// compressed, reported out of band. See
    /// [`Scrollback::image_rows_dropped_by_compression`].
    #[must_use]
    #[inline]
    pub fn image_rows_dropped_by_compression(&self) -> u64 {
        dispatch!(self, image_rows_dropped_by_compression())
    }

    /// Push a new line to the scrollback.
    // Skip: the tier-promotion driver — the blanket-drain/alloc class of the
    // tier operations it dispatches into (each individually classified).
    #[cfg_attr(trust_verify, trust::skip)]
    #[inline]
    pub fn push_line(&mut self, line: Line) -> std::io::Result<()> {
        dispatch!(self, push_line(line))
    }

    /// Clear all lines from scrollback.
    // Skip: dispatches the per-tier clears (drop-glue lane).
    #[cfg_attr(trust_verify, trust::skip)]
    #[inline]
    pub fn clear(&mut self) -> std::io::Result<()> {
        dispatch!(self, clear())
    }

    /// Remove the `n` most recent lines from scrollback.
    ///
    /// Per the Kitty unscroll spec: "The lines that have been scrolled into
    /// the scrollback buffer are removed from the scrollback buffer."
    ///
    /// Returns an error if decompression or I/O fails during line extraction;
    /// the scrollback is then unchanged.
    #[inline]
    pub fn remove_newest(&mut self, n: usize) -> Result<(), ScrollbackError> {
        dispatch!(self, remove_newest(n))
    }

    /// Drop the `n` oldest lines, keeping the newest `line_count() - n`;
    /// `n >= line_count()` clears the store.
    ///
    /// The store advances a `front_offset` through cold → warm → hot, dropping
    /// whole blocks/pages as they empty, with no decompression (O(1) amortized
    /// per line). The off-thread reflow reads its input from the front of the
    /// store it refills at the back; this releases consumed input as it goes,
    /// keeping its transient memory O(step budget) rather than O(history).
    // Skip: dispatches into the tier truncations (blanket-drain class), like
    // `set_line_limit` below.
    #[cfg_attr(trust_verify, trust::skip)]
    #[inline]
    pub fn truncate_oldest(&mut self, n: usize) -> Result<(), ScrollbackError> {
        if n == 0 {
            return Ok(());
        }
        let keep = self.line_count().saturating_sub(n);
        match self {
            ScrollbackStorage::Memory(sb) => sb.truncate(keep),
        }
    }

    /// Set the line limit.
    // Skip: dispatches into the tier truncations (blanket-drain class).
    #[cfg_attr(trust_verify, trust::skip)]
    #[inline]
    pub fn set_line_limit(&mut self, limit: Option<usize>) {
        dispatch!(self, set_line_limit(limit));
    }

    /// Set the memory budget (bytes).
    ///
    /// Returns `Err` if enforcement failed (e.g. corrupted data prevented
    /// eviction and usage still exceeds the new budget).
    // Skip: dispatches into the tier evictions (blanket-drain/alloc class).
    #[cfg_attr(trust_verify, trust::skip)]
    #[inline]
    pub fn set_memory_budget(&mut self, budget: usize) -> Result<(), ScrollbackError> {
        dispatch!(self, set_memory_budget(budget))
    }

    // --- Non-trait methods ---

    /// Iterate over all lines (oldest to newest).
    #[must_use]
    #[inline]
    pub fn iter(&self) -> ScrollbackStorageIter<'_> {
        ScrollbackStorageIter {
            storage: self,
            idx: 0,
            buf: std::collections::VecDeque::new(),
        }
    }

    /// Bulk segment read backing [`ScrollbackStorageIter`] — devirtualized
    /// dispatch to the concrete backend's `read_segment` (see the `dispatch!`
    /// rationale above).
    #[inline]
    pub(crate) fn read_segment(&self, idx: usize) -> crate::iter::SegmentResult {
        match self {
            ScrollbackStorage::Memory(sb) => sb.read_segment(idx),
        }
    }
}

impl Default for ScrollbackStorage {
    #[inline]
    fn default() -> Self {
        ScrollbackStorage::Memory(Scrollback::with_defaults())
    }
}

impl From<Scrollback> for ScrollbackStorage {
    #[inline]
    fn from(sb: Scrollback) -> Self {
        ScrollbackStorage::Memory(sb)
    }
}
