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

        // Binary search to find the page
        let Some(page_idx) = self.find_page(physical_idx) else {
            return Err(ScrollbackError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("in-range line index {idx} (physical {physical_idx}) has no backing page"),
            )));
        };

        // Get the line within the page
        let page_start = if page_idx == 0 {
            0
        } else {
            self.cumulative_lines[page_idx - 1]
        };
        let Some(line_in_page) = physical_idx.checked_sub(page_start) else {
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

    /// Find the page containing the given line index.
    pub(super) fn find_page(&self, line_idx: usize) -> Option<usize> {
        // Binary search through cumulative line counts
        match self.cumulative_lines.binary_search(&(line_idx + 1)) {
            Ok(idx) => Some(idx),
            Err(idx) => {
                if idx < self.cumulative_lines.len() {
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
