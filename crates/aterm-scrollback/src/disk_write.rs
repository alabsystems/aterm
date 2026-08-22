// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Write path for [`DiskColdTier`] — compressed page append.

use super::DiskColdTier;
use crate::disk_format::{PAGE_HEADER_SIZE, PageIndexEntry, len_to_u32};
use std::io::{self, Seek, SeekFrom, Write};

/// Compressed-append bytes between durability barriers (fsync + header
/// catch-up).
///
/// WHY a policy boundary instead of the old per-page barrier: the cold tier
/// receives one page per `block_size` (default 100) pushed lines, and the old
/// append path paid TWO `sync_data()` calls plus a whole-file munmap/re-mmap
/// per page — cost scaling with OUTPUT RATE (fsyncs) and TOTAL FILE SIZE
/// (VMA/PTE churn), on the ingest path, serialized under the terminal lock.
/// Those barriers were never load-bearing for crash consistency: `scan_pages`
/// rebuilds the index by scanning page headers, treats the on-disk
/// `page_count`/`line_count` fields as advisory preallocation hints (clamped),
/// and already discards torn/partial tails (#5917). What the per-page fsync
/// actually bought was a smaller power-loss window for the COLD tail — but the
/// newest `hot_limit + warm_limit` (~11k default) lines live in RAM tiers that
/// were never crash-durable at all, so history near the write head is lost on
/// power failure regardless of how eagerly cold pages sync. Deferring the
/// barrier to this byte boundary (plus close/clear/compaction/back-truncation,
/// and a best-effort sync in `Drop`) keeps an equivalent guarantee: after
/// recovery the store holds a valid prefix of the history, now up to
/// `SYNC_APPEND_BYTES` of compressed pages shorter on power loss. A clean
/// process exit still syncs everything via `Drop`, and kernel writeback bounds
/// the window in wall-clock time on every supported platform.
const SYNC_APPEND_BYTES: usize = 1 << 20;

impl DiskColdTier {
    /// Push compressed data from a warm block.
    ///
    /// The data is already Zstd compressed.
    ///
    /// Uses a transactional pattern: internal state (line_count, index,
    /// cumulative_lines, write_offset) is only updated after all writes
    /// succeed. On I/O failure the state remains consistent with the
    /// previously committed page (#7575).
    ///
    /// Durability is DEFERRED: no fsync and no header rewrite here — see
    /// [`SYNC_APPEND_BYTES`] and [`Self::sync_appends`]. The mmap is also left
    /// untouched: appends land at/beyond the mapped extent, and reads of pages
    /// past that extent are served positionally by `read_page_bytes` until the
    /// next load/compaction re-maps — the per-append whole-file unmap/remap is
    /// gone as a category, not shaved.
    pub(crate) fn push_compressed(
        &mut self,
        compressed: &[u8],
        line_count: usize,
    ) -> io::Result<()> {
        if compressed.is_empty() || line_count == 0 {
            return Ok(());
        }

        // A mapped view covers only bytes below the extent it was created
        // with, and appends write at `write_offset` >= that extent — EXCEPT
        // right after a torn-tail crash recovery, where `load` mapped the full
        // physical file but `scan_pages` rewound `write_offset` to the end of
        // the last complete page. Overwriting bytes a live view maps is not a
        // coherence bet we take on every platform, so drop the view for that
        // (rare, at-most-once-per-recovery) overlap case; the positional-read
        // fallback serves all cold reads until the next re-map.
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

            // --- I/O block: page header + page data. If any step fails, we
            // return early WITHOUT modifying in-memory state. There is no
            // durability barrier and no header-count rewrite here — both are
            // deferred to the sync-policy boundary below. The old
            // data-before-header write-ahead ordering (#5917) is moot once the
            // header is advisory: scan_pages never trusts it beyond a clamped
            // preallocation hint.
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

            // Deferred-durability bookkeeping. Saturating: byte totals of data
            // this process actually wrote, so the sum always fits in `usize` —
            // the saturation just discharges the strict gate's unconstrained-
            // input overflow counterexample (crate idiom).
            self.unsynced_append_bytes = self
                .unsynced_append_bytes
                .saturating_add(PAGE_HEADER_SIZE)
                .saturating_add(compressed.len());
            self.header_dirty = true;
            if self.unsynced_append_bytes >= SYNC_APPEND_BYTES {
                // Policy-boundary barrier. A failure here is NOT a push
                // failure: the page bytes are written and the in-memory state
                // is committed, so surfacing an error would make the eviction
                // driver restore the block to the warm tier and double-count
                // it. Log and let the next boundary (or close) retry.
                if let Err(e) = self.sync_appends() {
                    aterm_log::warn!("push_compressed: deferred sync failed ({e}), will retry");
                }
            }
        } else {
            // In-memory only mode - just update counts
            let entry = PageIndexEntry {
                offset: 0,
                compressed_size: len_to_u32(compressed.len()),
                line_count: len_to_u32(line_count),
            };
            self.index.push(entry);
            self.line_count = self.line_count.saturating_add(line_count);
            // Same live-last/absolute-base rule as the disk-backed branch.
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
    /// Idempotent and free when nothing is pending. Called from the byte-
    /// policy boundary in [`Self::push_compressed`], from `Drop`, and from the
    /// test-only `sync()`; the operations that write the header through their
    /// own synced paths (clear/compaction/back-truncation) call
    /// [`Self::mark_appends_synced`] instead. The header counters are ADVISORY
    /// (`scan_pages` rebuilds and clamps them), so no write-ahead ordering is
    /// needed between the header write and the page data — one `sync_data`
    /// covers both, replacing the old two-barrier-per-page scheme.
    // Skip: seek/write/sync over an OS handle — the io class the strict gate
    // treats as absent-callee. Counter resets are plain stores.
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
