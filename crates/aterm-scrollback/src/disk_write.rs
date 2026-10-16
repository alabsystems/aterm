// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Write path for [`DiskColdTier`] — compressed page append.

use super::DiskColdTier;
use crate::disk_format::{PAGE_HEADER_SIZE, PageIndexEntry, len_to_u32};
use std::io::{self, Seek, SeekFrom, Write};

/// Compressed-append bytes between durability barriers (fsync + header catch-up).
///
/// A per-page barrier would put two `sync_data()` calls per page on the ingest
/// path, under the terminal lock, without buying crash consistency: `scan_pages`
/// rebuilds the index from page headers, treats the header's
/// `page_count`/`line_count` as clamped hints, and discards torn tails. The newest
/// `hot_limit + warm_limit` lines (~11k by default) live in RAM and are lost on
/// power failure anyway. With barriers here (plus close, clear, compaction,
/// back-truncation and `Drop`), a recovered store holds a valid prefix of the
/// history, at most `SYNC_APPEND_BYTES` of compressed pages short.
const SYNC_APPEND_BYTES: usize = 1 << 20;

impl DiskColdTier {
    /// Push cold-codec (LZ4) compressed data from a warm block.
    ///
    /// Transactional: line_count, index, cumulative_lines and write_offset change
    /// only after all writes succeed, so an I/O failure leaves the last committed
    /// state.
    ///
    /// Durability is deferred: no fsync or header rewrite here (see
    /// [`SYNC_APPEND_BYTES`] and [`Self::sync_appends`]). The mmap is not touched:
    /// pages past its extent are read positionally by `read_page_bytes` until the
    /// next load or compaction re-maps.
    pub(crate) fn push_compressed(
        &mut self,
        compressed: &[u8],
        line_count: usize,
    ) -> io::Result<()> {
        if compressed.is_empty() || line_count == 0 {
            return Ok(());
        }

        // Appends write at or past the mapped extent, except right after torn-tail
        // recovery: `load` mapped the whole file but `scan_pages` rewound
        // `write_offset` to the end of the last complete page. Drop the view in that
        // rare overlap rather than overwrite mapped bytes; positional reads serve
        // cold reads until the next re-map.
        let stale_overlap = self
            .mmap
            .as_ref()
            .is_some_and(|m| self.write_offset < m.len() as u64);
        if stale_overlap {
            self.mmap = None;
        }

        if let Some(ref mut file) = self.file {
            // Prepare values for the transactional update.
            let new_write_offset = self
                .write_offset
                .saturating_add(PAGE_HEADER_SIZE as u64)
                .saturating_add(compressed.len() as u64);
            let new_line_count = self.line_count.saturating_add(line_count);
            let new_cumulative = self
                .cumulative_lines
                .last()
                .copied()
                .unwrap_or(self.cumulative_base)
                .saturating_add(line_count);
            let entry = PageIndexEntry {
                offset: self.write_offset,
                compressed_size: len_to_u32(compressed.len()),
                line_count: len_to_u32(line_count),
            };

            // Page header + data. A failure returns before any in-memory change.
            // No barrier or header-count rewrite (deferred, below), and no
            // data-before-header ordering: scan_pages treats the header as a
            // clamped hint.
            let mut page_header = [0u8; PAGE_HEADER_SIZE];
            page_header[0..4].copy_from_slice(&len_to_u32(compressed.len()).to_le_bytes());
            page_header[4..8].copy_from_slice(&len_to_u32(line_count).to_le_bytes());

            file.seek(SeekFrom::Start(self.write_offset))?;
            file.write_all(&page_header)?;
            file.write_all(compressed)?;

            // --- All writes succeeded. Commit in-memory state atomically. ---
            self.line_count = new_line_count;
            self.write_offset = new_write_offset;
            self.index.push(entry);
            self.cumulative_lines.push(new_cumulative);

            // Deferred-durability bookkeeping. Saturating discharges the strict
            // gate's overflow obligation; bytes actually written always fit.
            self.unsynced_append_bytes = self
                .unsynced_append_bytes
                .saturating_add(PAGE_HEADER_SIZE)
                .saturating_add(compressed.len());
            self.header_dirty = true;
            if self.unsynced_append_bytes >= SYNC_APPEND_BYTES {
                // A failed barrier is not a push failure: the page is written and
                // committed, and an error would make the eviction driver restore
                // the block to the warm tier and double-count it. The next
                // boundary (or close) retries.
                if let Err(e) = self.sync_appends() {
                    aterm_log::warn!("push_compressed: deferred sync failed ({e}), will retry");
                }
            }
        } else {
            // In-memory mode: update the counts only.
            let entry = PageIndexEntry {
                offset: 0,
                compressed_size: len_to_u32(compressed.len()),
                line_count: len_to_u32(line_count),
            };
            self.index.push(entry);
            self.line_count = self.line_count.saturating_add(line_count);
            // As in the file branch: `last()` is the last live entry (the dead
            // prefix is at the front); when empty, the base is the absolute floor.
            let cumulative = self
                .cumulative_lines
                .last()
                .copied()
                .unwrap_or(self.cumulative_base)
                .saturating_add(line_count);
            self.cumulative_lines.push(cumulative);
        }

        self.reset_bytes_used();

        Ok(())
    }

    /// Bring the on-disk header counters up to date and issue the append-side
    /// durability barrier.
    ///
    /// Free when nothing is pending. Called from the byte boundary in
    /// [`Self::push_compressed`], from `Drop`, and from the test-only `sync()`; paths
    /// that write the header through their own synced I/O (clear, compaction,
    /// back-truncation) call [`Self::mark_appends_synced`] instead. The header
    /// counters are advisory (`scan_pages` rebuilds and clamps them), so one
    /// `sync_data` covers header and page data with no write-ahead ordering.
    // Skip: seek/write/sync over an OS handle, the io class the strict gate treats
    // as absent-callee.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn sync_appends(&mut self) -> io::Result<()> {
        if !self.header_dirty && self.unsynced_append_bytes == 0 {
            return Ok(());
        }
        let Some(ref mut file) = self.file else {
            // In-memory mode has nothing to make durable.
            self.header_dirty = false;
            self.unsynced_append_bytes = 0;
            return Ok(());
        };
        if self.header_dirty {
            file.seek(SeekFrom::Start(8))?;
            file.write_all(&(self.index.len() as u64).to_le_bytes())?;
            file.write_all(&(self.line_count as u64).to_le_bytes())?;
        }
        file.sync_data()?;
        self.header_dirty = false;
        self.unsynced_append_bytes = 0;
        Ok(())
    }

    /// Mark the deferred-append state clean after an operation that already
    /// wrote the header counters and synced through its own path (clear,
    /// compaction reopen, back-truncation).
    pub(super) fn mark_appends_synced(&mut self) {
        self.header_dirty = false;
        self.unsynced_append_bytes = 0;
    }
}
