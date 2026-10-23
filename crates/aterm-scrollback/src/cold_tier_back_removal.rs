// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Back removal for [`ColdTier`], behind `remove_newest`.

use super::{ColdPage, ColdTier};

impl ColdTier {
    /// Removes the newest `n` lines from the back of the cold tier.
    ///
    /// # Panics
    ///
    /// Debug-asserts that `n <= self.line_count`.
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

        let mut whole_pages = 0;
        let mut remaining = n;
        for page in self.pages.iter().rev() {
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
                    lost: bp.lost,
                })
            }
        } else {
            None
        };

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

        if self.pages.is_empty() {
            self.front_offset = 0;
        }

        self.cumulative_lines.clear();
        self.cum_start = 0;
        self.cumulative_base = 0;
        let mut total = 0;
        for page in &self.pages {
            total += page.line_count;
            self.cumulative_lines.push(total);
        }
        self.clear_cache();

        Ok(())
    }
}
