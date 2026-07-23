// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates
//! Truncation helpers for [`DiskBackedScrollback`].
use super::*;

impl DiskBackedScrollback {
    /// Truncate to keep only the last `n` lines.
    ///
    /// Uses tier-aware removal: calculates per-tier removal amounts and
    /// processes them directly. Cold tier uses `front_offset` for O(1)
    /// removal without decompression. Only the warm tier boundary block
    /// (at most one LZ4 decompress) is fallible.
    ///
    /// Error safety: warm tier (only fallible step) runs first. If it fails,
    /// neither the cold tier nor hot tier have been modified (#4638).
    pub(crate) fn truncate(&mut self, n: usize) -> Result<(), ScrollbackError> {
        if n == 0 {
            self.clear()?;
            return Ok(());
        }
        if n >= self.line_count {
            return Ok(());
        }

        let to_remove = self.line_count - n;

        // Calculate per-tier removal amounts up front (oldest removed first).
        let cold_lines = self.cold.line_count();
        let cold_remove = to_remove.min(cold_lines);
        let after_cold = to_remove - cold_remove;

        let warm_lines = self.warm.line_count();
        let warm_remove = after_cold.min(warm_lines);
        let hot_remove = after_cold - warm_remove;

        // Phase 1: Warm tier first — only fallible step (boundary block LZ4 decompress).
        // If this fails, no state has been modified yet.
        if warm_remove > 0 {
            self.warm.truncate_front_lines(warm_remove)?;
        }

        // Phase 2: Cold tier — infallible (O(1) front_offset, no decompression).
        if cold_remove > 0 {
            self.cold.truncate_front_lines(cold_remove);
        }

        // Phase 3: Hot tier — infallible.
        if hot_remove > 0 {
            let hot_keep = self.hot.len().saturating_sub(hot_remove);
            self.hot.truncate_front(hot_keep);
        }

        self.line_count = n;
        self.sync_accounting();
        self.assert_bytes_used_invariant();
        Ok(())
    }

    /// Remove the `n` most recent lines (Kitty unscroll spec).
    ///
    /// Uses tier-aware back-removal: removes from hot first, then warm, then
    /// cold — decompressing at most one boundary block/page at a time. This
    /// bounds peak memory to ~one block regardless of total scrollback size.
    ///
    /// Returns an error if decompression or I/O fails; state unchanged on error (#4638).
    pub(crate) fn remove_newest(&mut self, n: usize) -> Result<(), ScrollbackError> {
        if n == 0 || self.line_count == 0 {
            return Ok(());
        }
        if n >= self.line_count {
            self.clear()?;
            return Ok(());
        }

        // Calculate per-tier removal amounts.
        let hot_remove = n.min(self.hot.len());
        let after_hot = n - hot_remove;
        let warm_remove = after_hot.min(self.warm.line_count());
        let cold_remove = after_hot - warm_remove;

        // Pre-validate all fallible decompressions before modifying state (#4638).
        if warm_remove > 0 {
            self.warm.pre_validate_truncate_back(warm_remove)?;
        }
        if cold_remove > 0 {
            self.cold.pre_validate_truncate_back(cold_remove)?;
        }

        // Commit. Pre-validation has confirmed every boundary block/page
        // DECOMPRESSES, but the re-compress and (for the cold tier) the file
        // write/sync/set_len are still fallible — pre-validation cannot exercise
        // them. Propagate any such error with `?` instead of panicking. Each
        // tier's `truncate_back_lines` is internally transactional (state
        // unchanged on its own error, #4638), so a failure leaves that tier and
        // `self.line_count` consistent for the caller to recover from.
        // Decrement the aggregate `line_count` INCREMENTALLY as each tier's
        // truncation commits, so a later tier's I/O error (the warm re-compress
        // and the cold file write/sync/set_len stay fallible even after
        // pre-validation) leaves `line_count` matching exactly the tiers that
        // succeeded — a bulk `-= n` only on full success would overstate the
        // removal on a partial failure and desync the aggregate from the tiers.
        // Newest-first order (hot→warm→cold) means a partial failure trims the
        // newest lines and leaves only the oldest boundary portion in place.
        if hot_remove > 0 {
            self.hot.truncate_back(hot_remove);
            self.line_count = self.line_count.saturating_sub(hot_remove);
        }
        if warm_remove > 0 {
            self.warm.truncate_back_lines(warm_remove)?;
            self.line_count = self.line_count.saturating_sub(warm_remove);
        }
        if cold_remove > 0 {
            self.cold.truncate_back_lines(cold_remove)?;
            self.line_count = self.line_count.saturating_sub(cold_remove);
        }
        self.sync_accounting();
        self.assert_bytes_used_invariant();
        Ok(())
    }
}
