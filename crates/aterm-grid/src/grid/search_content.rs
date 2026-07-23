// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `SearchContent` adapter for `Grid`.
//!
//! Moved from aterm-core as part of the grid ownership transfer (#6554).
//! The orphan rule requires this impl to live in the same crate as `Grid`.

use super::Grid;
use aterm_types::SearchContent;

impl SearchContent for Grid {
    fn row_count(&self) -> usize {
        self.storage.scrollback_lines() + usize::from(self.rows())
    }

    fn get_row_text(&mut self, row: usize) -> Option<String> {
        let scrollback_lines = self.storage.scrollback_lines();
        if row < scrollback_lines {
            return match self.try_get_history_line(row) {
                Ok(Some(line)) => Some(line.to_string()),
                Ok(None) => None,
                Err(error) => {
                    aterm_log::warn!("Grid::get_row_text({row}) failed: {error}");
                    None
                }
            };
        }

        let visible_idx = row.saturating_sub(scrollback_lines);
        if visible_idx >= usize::from(self.rows()) {
            return None;
        }

        let row_u16 = u16::try_from(visible_idx).ok()?;
        self.row_text(row_u16)
    }

    fn is_row_wrapped(&self, row: usize) -> bool {
        let scrollback_lines = self.storage.scrollback_lines();
        if row < scrollback_lines {
            return match self.try_get_history_line(row) {
                Ok(Some(line)) => line.is_wrapped(),
                _ => false,
            };
        }

        let visible_idx = row.saturating_sub(scrollback_lines);
        let row_u16 = match u16::try_from(visible_idx) {
            Ok(v) => v,
            Err(_) => return false,
        };
        self.row(row_u16).is_some_and(|r| r.is_wrapped())
    }
}
