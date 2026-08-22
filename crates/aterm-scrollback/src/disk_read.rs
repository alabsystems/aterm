// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Read path for [`DiskColdTier`] — line lookup and page cache.

use super::DiskColdTier;
use crate::ScrollbackError;
use crate::line::Line;
use std::io;

impl DiskColdTier {
    /// Get a line by index (0 = oldest available line, accounting for front_offset).
    ///
    /// Takes `&self` despite updating the LRU cache internally, because the
    /// cache fields use interior mutability (`RefCell`/`Cell`).
    ///
    /// Returns `Ok(None)` for out-of-bounds, `Err` for I/O or decompression failures.
    pub(crate) fn get_line(&self, idx: usize) -> Result<Option<Line>, ScrollbackError> {
        if idx >= self.line_count {
            return Ok(None);
        }

        // Translate logical index (0 = oldest available) to physical index
        // (0 = first line in first page, including consumed lines).
        let physical_idx = idx + self.front_offset;

        // Binary search to find the page + offset within it.
        let (page_idx, line_in_page) = self.locate(physical_idx)?;

        // Load single line from page (possibly from cache)
        let Some(line) = self.load_line(page_idx, line_in_page)? else {
            return Err(ScrollbackError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "page {page_idx} missing line offset {line_in_page} for global index {idx}"
                ),
            )));
        };
        Ok(Some(line))
    }

    /// Resolve a PHYSICAL line index to `(page_idx, line_in_page)`.
    ///
    /// The single home for the cumulative-index geometry, shared by the
    /// random-access read path (`get_line`) and the block-streaming bulk
    /// path (`take_lines_from`/`segment_len_at`) so the two cannot drift.
    /// Errors carry the same corruption semantics the inline math had:
    /// a missing page for an in-range index, or a page start exceeding the
    /// physical index (desynchronized index), are bad on-disk data.
    fn locate(&self, physical_idx: usize) -> Result<(usize, usize), ScrollbackError> {
        let Some(page_idx) = self.find_page(physical_idx) else {
            return Err(ScrollbackError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("physical line index {physical_idx} has no backing page"),
            )));
        };
        let page_start = if page_idx == 0 {
            self.cumulative_base
        } else {
            let Some(&prev) = self.live_cumulative().get(page_idx - 1) else {
                return Err(ScrollbackError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "cumulative index missing live entry {} for page {page_idx}",
                        page_idx - 1
                    ),
                )));
            };
            prev
        };
        let Some(line_in_page) = physical_idx
            .checked_add(self.cumulative_base)
            .and_then(|abs| abs.checked_sub(page_start))
        else {
            // find_page returned a page whose start exceeds the physical index.
            // This can only happen if the index/cumulative_lines are corrupt or
            // desynchronized; treat it as bad on-disk data rather than wrapping.
            return Err(ScrollbackError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "page {page_idx} start {page_start} exceeds physical line index {physical_idx}"
                ),
            )));
        };
        Ok((page_idx, line_in_page))
    }

    /// Decode the page containing logical line `idx` and return OWNED lines
    /// from `idx` through the end of that page — the bulk-walk primitive
    /// (ST-6). One decode + one `split_off` per page: no per-line binary
    /// search, no per-line `Line` clone, and NO insertion into the LRU page
    /// cache (a full-history walk must not evict the pages the viewport is
    /// reading; `decompress_page` alone does not cache).
    ///
    /// Returns an empty vec for an out-of-bounds `idx`.
    pub(crate) fn take_lines_from(&self, idx: usize) -> Result<Vec<Line>, ScrollbackError> {
        if idx >= self.line_count {
            return Ok(Vec::new());
        }
        let physical_idx = idx + self.front_offset;
        let (page_idx, line_in_page) = self.locate(physical_idx)?;
        let mut lines = self.decompress_page(page_idx)?;
        // `min` keeps split_off total; a short decode yields a short
        // (possibly empty) segment, which the streaming iterator treats as
        // end-of-data — fail-closed, never a panic.
        let split_at = line_in_page.min(lines.len());
        Ok(lines.split_off(split_at))
    }

    /// Logical lines from `idx` through the end of its containing page —
    /// how far a bulk walk skips when that page fails to decode (its whole
    /// remaining span, matching the per-line skip total of the old
    /// line-at-a-time walk). Zero when out of bounds. Never decodes.
    pub(crate) fn segment_len_at(&self, idx: usize) -> usize {
        if idx >= self.line_count {
            return 0;
        }
        let physical_idx = idx + self.front_offset;
        let Ok((page_idx, _)) = self.locate(physical_idx) else {
            return 0;
        };
        // ABSOLUTE live entry minus the dropped-prefix base (ST-3 base-offset
        // index): yields the physical end relative to the live front, exactly
        // what the pre-ST-3 relative entry held.
        let page_end = self
            .live_cumulative()
            .get(page_idx)
            .copied()
            .unwrap_or(self.cumulative_base)
            .saturating_sub(self.cumulative_base);
        // Physical page end -> logical, clamped by the tier's logical count
        // (the last page's physical end IS the logical end), minus `idx`.
        page_end
            .saturating_sub(self.front_offset)
            .min(self.line_count)
            .saturating_sub(idx)
    }

    /// Find the page containing the given line index.
    pub(super) fn find_page(&self, line_idx: usize) -> Option<usize> {
        // Binary search through the LIVE cumulative entries for the ABSOLUTE
        // target: stored values are never rebased on front drops, so the
        // dropped-prefix base is added to the query instead. Live indices
        // are exactly the live page indices.
        let live = self.live_cumulative();
        let target = line_idx.saturating_add(self.cumulative_base).saturating_add(1);
        match live.binary_search(&target) {
            Ok(idx) => Some(idx),
            Err(idx) => {
                if idx < live.len() {
                    Some(idx)
                } else {
                    None
                }
            }
        }
    }

    /// Load a single line from a page (from cache or disk).
    ///
    /// Extracts one line without cloning the entire page Vec on cache hits.
    /// Uses interior mutability to update the LRU cache while taking `&self`.
    fn load_line(
        &self,
        page_idx: usize,
        line_in_page: usize,
    ) -> Result<Option<Line>, ScrollbackError> {
        // Check cache first — borrow and extract single line
        {
            let mut cache = self.cache.borrow_mut();
            if let Some(entry) = cache.get_mut(&page_idx) {
                let counter = self.access_counter.get() + 1;
                self.access_counter.set(counter);
                entry.last_access = counter;
                return Ok(entry.lines.get(line_in_page).cloned());
            }
        }

        // Cache miss: decompress, extract line, then cache (no extra clone)
        // decompress_page and cache_page are in disk_memory.rs
        let lines = self.decompress_page(page_idx)?;
        let line = lines.get(line_in_page).cloned();
        self.cache_page(page_idx, lines);

        Ok(line)
    }
}
