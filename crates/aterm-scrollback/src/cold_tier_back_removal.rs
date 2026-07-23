// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Back-removal operations for [`ColdTier`].
//!
//! Extracted from `cold_tier.rs` for file-size compliance.
//! These methods support `remove_newest` (#5918).

use super::{ColdPage, ColdTier};

impl ColdTier {
    /// Pre-validate that `truncate_back_lines(n)` will succeed.
    ///
    /// Tries to decompress the boundary page (if any) without modifying state.
    /// Call this before committing cross-tier removal to ensure error safety.
    // Skip: the reverse page walk drives std iterator `next` (absent body) —
    // the twin of tier_back_removal's. Pure validation; ARENA-SCROLL tested.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn pre_validate_truncate_back(
        &self,
        n: usize,
    ) -> Result<(), crate::ScrollbackError> {
        if n == 0 || n >= self.line_count {
            return Ok(()); // No boundary page; whole-page removal is infallible.
        }
        let mut remaining = n;
        let mut whole_pages = 0;
        for page in self.pages.iter().rev() {
            // saturating index/arith + total `get` — the exact twin of
            // `tier_back_removal::pre_validate_truncate_back`: the guards prove
            // the bounds but do not reach the ops in the verifier's model.
            // Identical on every real path.
            let actual_idx = self
                .pages
                .len()
                .saturating_sub(1)
                .saturating_sub(whole_pages);
            let available = if actual_idx == 0 {
                page.line_count.saturating_sub(self.front_offset)
            } else {
                page.line_count
            };
            if remaining >= available {
                remaining = remaining.saturating_sub(available);
                whole_pages = whole_pages.saturating_add(1);
            } else {
                break;
            }
        }
        if remaining > 0 && whole_pages < self.pages.len() {
            let boundary_idx = self
                .pages
                .len()
                .saturating_sub(1)
                .saturating_sub(whole_pages);
            if let Some(p) = self.pages.get(boundary_idx) {
                p.decompress()?;
            }
        }
        Ok(())
    }

    /// Remove the newest `n` lines from the back of the cold tier.
    ///
    /// Drops whole pages from the back without decompression. For the
    /// boundary page (partially within the remove range), decompresses it,
    /// trims the consumed lines from the back, and re-compresses the survivors.
    ///
    /// Error safety (#4638): the boundary page is decompressed before any state
    /// is modified. On decompression failure, state is unchanged.
    ///
    /// # Panics
    ///
    /// Debug-asserts that `n <= self.line_count`.
    // Skip: the residue is the blanket-unmodeled drain/iterator class plus the
    // boundary rebuild's alloc — the arithmetic is now saturating and proven
    // (twin of the warm tier's). ARENA-SCROLL tested.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(crate) fn truncate_back_lines(&mut self, n: usize) -> Result<(), crate::ScrollbackError> {
        if n == 0 {
            return Ok(());
        }
        debug_assert!(
            n <= self.line_count,
            "truncate_back_lines({n}) exceeds line_count({})",
            self.line_count
        );

        // Phase 1: Count whole pages to drop from the back and identify boundary.
        let mut whole_pages = 0;
        let mut remaining = n;
        for page in self.pages.iter().rev() {
            // saturating index/arith — the exact twin of the warm tier's commit
            // path: the guards prove the bounds but do not reach the ops in the
            // verifier's model. Identical on every real path.
            let page_idx_from_back = whole_pages;
            let actual_idx = self
                .pages
                .len()
                .saturating_sub(1)
                .saturating_sub(page_idx_from_back);
            let available = if actual_idx == 0 {
                page.line_count.saturating_sub(self.front_offset)
            } else {
                page.line_count
            };
            if remaining >= available {
                remaining = remaining.saturating_sub(available);
                whole_pages = whole_pages.saturating_add(1);
            } else {
                break;
            }
        }
        let boundary_trim = remaining;

        // Phase 2: Pre-decompress boundary page if needed (before modifying state).
        let boundary_replacement = if boundary_trim > 0 {
            let boundary_idx = self
                .pages
                .len()
                .saturating_sub(1)
                .saturating_sub(whole_pages);
            let Some(bp) = self.pages.get(boundary_idx) else {
                return Ok(());
            };
            let lines = bp.decompress()?;
            debug_assert!(
                lines.len() >= boundary_trim,
                "decompress returned {} lines but boundary_trim is {}",
                lines.len(),
                boundary_trim,
            );
            let keep = lines.len().saturating_sub(boundary_trim);
            if keep == 0 {
                None
            } else {
                let serialized = crate::line::serialize_lines(&lines[..keep]);
                let compressed = crate::encode_cold_block(&serialized)?;
                Some(ColdPage {
                    compressed,
                    line_count: keep,
                })
            }
        } else {
            None
        };

        // Phase 3: Commit — all decompressions succeeded.
        for _ in 0..whole_pages {
            if let Some(page) = self.pages.pop_back() {
                self.bytes_used = self.bytes_used.saturating_sub(page.compressed.len());
            }
        }

        if boundary_trim > 0 && !self.pages.is_empty() {
            if let Some(old_page) = self.pages.pop_back() {
                self.bytes_used = self.bytes_used.saturating_sub(old_page.compressed.len());
            }
            if let Some(replacement) = boundary_replacement {
                self.bytes_used += replacement.compressed.len();
                self.pages.push_back(replacement);
            }
        }

        if n > self.line_count {
            aterm_log::warn!(
                "cold truncate_back_lines({n}) exceeds line_count({}), saturating",
                self.line_count
            );
        }
        self.line_count = self.line_count.saturating_sub(n);

        // Reset front_offset when all pages are gone. Without this, a stale
        // front_offset would incorrectly skip lines from the first page if
        // new pages are appended later. Matches warm tier cleanup in
        // tier_back_removal.rs.
        if self.pages.is_empty() {
            self.front_offset = 0;
        }

        // Rebuild cumulative index from pages.
        self.cumulative_lines.clear();
        let mut total = 0;
        for page in &self.pages {
            total += page.line_count;
            self.cumulative_lines.push(total);
        }
        *self.last_page_cache.borrow_mut() = None;

        Ok(())
    }
}
