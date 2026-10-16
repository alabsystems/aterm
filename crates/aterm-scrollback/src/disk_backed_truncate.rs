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
    /// Selects the newest lines across hot, warm and cold tiers, committing
    /// the oldest affected tier first. Decompresses at most one boundary
    /// block/page at a time, bounding peak memory to ~one block regardless of
    /// total scrollback size.
    ///
    /// On decompression or I/O failure, newer RAM tiers and aggregate counters
    /// remain unchanged. A partial disk write can still damage the cold boundary
    /// page; the cold tier's reads fail closed for affected bytes (#4638).
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

        // Cold I/O must finish before touching the RAM tiers. If cold is
        // affected, every warm block is removed whole (infallible); otherwise
        // the warm boundary is the only fallible step and precedes hot removal.
        // Each tier prepares its boundary before mutation, without a separate
        // validation pass.
        // This order preserves newer RAM tiers and aggregate counters when the
        // boundary operation fails. It does not roll back partial disk writes;
        // the cold tier retains its existing fail-closed read behavior.
        if cold_remove > 0 {
            self.cold.truncate_back_lines(cold_remove)?;
        }
        if warm_remove > 0 {
            self.warm.truncate_back_lines(warm_remove)?;
        }
        if hot_remove > 0 {
            self.hot.truncate_back(hot_remove);
        }
        self.line_count = self.line_count.saturating_sub(n);
        self.sync_accounting();
        self.assert_bytes_used_invariant();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_cold_write_preserves_newer_tiers_and_their_accounting() {
        for cold_remove in [1, 5] {
            let dir = aterm_tempfile::tempdir().expect("scratch directory");
            let path = dir.path().join("remove-newest.dtrm");
            let config = DiskBackedScrollbackConfig::new(&path)
                .with_hot_limit(5)
                .with_warm_limit(10)
                .with_block_size(5);
            let mut sb = DiskBackedScrollback::with_config(config).expect("store");
            for row in 0..50 {
                sb.push_str(&format!("row {row}")).expect("append");
            }
            let tiers = (
                sb.hot_line_count(),
                sb.warm_line_count(),
                sb.cold_line_count(),
            );
            assert!(tiers.0 > 0 && tiers.1 > 0 && tiers.2 > cold_remove);
            let count = sb.line_count();
            let budgeted = sb.budgeted_memory_used();
            sb.cold
                .make_file_read_only_for_test()
                .expect("read-only file");

            sb.remove_newest(tiers.0 + tiers.1 + cold_remove)
                .expect_err("the real cold write must fail");

            assert_eq!(sb.line_count(), count, "a failed removal loses no lines");
            assert_eq!(
                (
                    sb.hot_line_count(),
                    sb.warm_line_count(),
                    sb.cold_line_count()
                ),
                tiers,
                "RAM tiers must survive a cold I/O failure"
            );
            assert_eq!(sb.budgeted_memory_used(), budgeted);
            sb.assert_bytes_used_invariant();
            for row in 0..count {
                assert_eq!(
                    sb.get_line(row)
                        .expect("read")
                        .expect("retained")
                        .to_string(),
                    format!("row {row}")
                );
            }
        }
    }
}
