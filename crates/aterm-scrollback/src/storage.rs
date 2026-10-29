// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::borrow::Cow;

use super::access::ScrollbackAccess;
use super::{ImageSpan, Line, Scrollback, ScrollbackError, WatermarkLevel};

/// Iterator over scrollback storage lines (oldest to newest).
pub struct ScrollbackStorageIter<'a> {
    pub(crate) storage: &'a ScrollbackStorage,
    pub(crate) idx: usize,
    /// Decoded lines of the current segment, drained front-to-back.
    pub(crate) buf: std::collections::VecDeque<Line>,
}

impl Iterator for ScrollbackStorageIter<'_> {
    type Item = Line;

    #[cfg_attr(trust_verify, trust::skip)]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(line) = self.buf.pop_front() {
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

macro_rules! dispatch {
    ($self:expr, $method:ident($($arg:expr),*)) => {
        match $self {
            ScrollbackStorage::Memory(sb) => ScrollbackAccess::$method(sb $(, $arg)*),
        }
    };
}

impl ScrollbackStorage {
    /// Whether retained history holds an image accepted by `selected`.
    /// Compressed tiers contain no image spans.
    #[must_use]
    pub fn any_image(&self, selected: &dyn Fn(&ImageSpan) -> bool) -> bool {
        match self {
            Self::Memory(sb) => sb.hot.any_image(selected),
        }
    }

    /// Inline-image spans on uncompressed line `idx` (`0` = oldest across all tiers), if any.
    /// Compressed warm and cold lines carry no images and return `None` without decompressing.
    #[must_use]
    pub fn line_images(&self, idx: usize) -> Option<&[ImageSpan]> {
        match self {
            Self::Memory(sb) => {
                let non_hot = sb.cold.line_count().saturating_add(sb.warm.line_count());
                let hot_idx = idx.checked_sub(non_hot)?;
                sb.hot.line_images(hot_idx)
            }
        }
    }

    /// Replaces inline-image spans on hot line `rev_idx` (`0` = newest in hot tier), reconciling byte counts.
    /// Returns `false` when `rev_idx` falls in a compressed tier.
    pub fn set_hot_line_images(&mut self, rev_idx: usize, spans: Vec<ImageSpan>) -> bool {
        match self {
            Self::Memory(sb) => {
                let changed = sb.hot.set_line_images_rev(rev_idx, spans);
                if changed {
                    sb.sync_accounting();
                    sb.assert_bytes_used_invariant();
                }
                changed
            }
        }
    }

    /// Retains accepted image spans without changing history's text, order or retention horizon.
    /// Returns whether any image span was removed.
    pub fn retain_images(&mut self, keep: &mut dyn FnMut(&ImageSpan) -> bool) -> bool {
        match self {
            Self::Memory(sb) => {
                let changed = sb.hot.retain_images(keep);
                if changed {
                    sb.sync_accounting();
                    sb.assert_bytes_used_invariant();
                }
                changed
            }
        }
    }

    /// Get the total number of lines.
    #[must_use]
    #[inline]
    pub fn line_count(&self) -> usize {
        dispatch!(self, line_count())
    }

    /// Get a line by index (0 = oldest).
    #[must_use = "line data is discarded if not consumed"]
    #[inline]
    pub fn get_line(&self, idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError> {
        dispatch!(self, get_line(idx))
    }

    /// Get a line by reverse index (0 = newest).
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
    #[must_use]
    #[inline]
    pub fn watermark_level(&self) -> WatermarkLevel {
        dispatch!(self, watermark_level())
    }

    /// Lines dropped by memory-pressure eviction: real retention loss, reported out of band.
    #[must_use]
    #[inline]
    pub const fn pressure_evicted_lines(&self) -> u64 {
        match self {
            ScrollbackStorage::Memory(sb) => sb.pressure_evicted_lines(),
        }
    }

    /// History lines whose inline image was discarded because the line was compressed.
    #[must_use]
    #[inline]
    pub fn image_rows_dropped_by_compression(&self) -> u64 {
        dispatch!(self, image_rows_dropped_by_compression())
    }

    /// Push a new line to the scrollback.
    #[cfg_attr(trust_verify, trust::skip)]
    #[inline]
    pub fn push_line(&mut self, line: Line) -> std::io::Result<()> {
        dispatch!(self, push_line(line))
    }

    /// Clear all lines from scrollback.
    #[cfg_attr(trust_verify, trust::skip)]
    #[inline]
    pub fn clear(&mut self) -> std::io::Result<()> {
        dispatch!(self, clear())
    }

    /// Remove the `n` most recent lines from scrollback.
    #[inline]
    pub fn remove_newest(&mut self, n: usize) -> Result<(), ScrollbackError> {
        dispatch!(self, remove_newest(n))
    }

    /// Drop the `n` oldest lines, keeping the newest `line_count() - n`.
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
    #[cfg_attr(trust_verify, trust::skip)]
    #[inline]
    pub fn set_line_limit(&mut self, limit: Option<usize>) {
        dispatch!(self, set_line_limit(limit));
    }

    /// Set the memory budget (bytes).
    #[cfg_attr(trust_verify, trust::skip)]
    #[inline]
    pub fn set_memory_budget(&mut self, budget: usize) -> Result<(), ScrollbackError> {
        dispatch!(self, set_memory_budget(budget))
    }

    /// Iterate over all lines (oldest to newest).
    #[must_use]
    #[inline]
    pub const fn iter(&self) -> ScrollbackStorageIter<'_> {
        ScrollbackStorageIter {
            storage: self,
            idx: 0,
            buf: std::collections::VecDeque::new(),
        }
    }

    /// Bulk segment read backing [`ScrollbackStorageIter`].
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
