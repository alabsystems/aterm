// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal grid with O(1) display scrolling and O(n × cols) content scrolling.

mod accessors;
mod anchor_hooks;
mod anchor_lend;
mod checkpoint_lines;
mod construct;
mod content_queries;
mod cursor_ops;
mod erase;
mod history_carry;
mod invariants;
mod line_ops;
mod pin_methods;
pub mod reflow;
mod reflow_pullback;
mod resize_handback;
mod resize_undo;
mod ring_extras;
mod scroll;
pub mod scroll_convert;
mod scroll_fill;
pub mod scroll_materialize;
mod scroll_unscroll;
mod scrollback_access;
mod scrollback_offload;
mod scrollback_reflow;
pub mod state;
mod tab_ops;
mod viewport_row_cache;
mod visible_row_view;
mod write;
mod write_split;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod write_test_helpers;

pub(in crate::grid) use crate::Cursor;
#[cfg(test)]
pub(in crate::grid) use crate::PackedColor;
pub(in crate::grid) use crate::row_u16;
pub(in crate::grid) use crate::{Cell, CellFlags, PackedColors};
pub(in crate::grid) use crate::{CellCoord, CellExtra, CellExtras};
pub(in crate::grid) use crate::{HorizontalMargins, PAGE_SIZE, PageStore, ScrollRegion};
pub(in crate::grid) use crate::{LineSize, Row};

#[cfg(any(test, debug_assertions))]
pub(in crate::grid) use crate::test_counters::{
    count_reflow_passthrough_lines, count_reflow_row_op, count_ring_fast_materialize,
    count_row_to_line_op, count_rows_only_resize_migrated_rows, count_scrollback_reflow_sync_lines,
    count_viewport_row_materialize,
};

#[cfg(test)]
pub(in crate::grid) use aterm_scrollback::Scrollback;
#[cfg(test)]
pub(in crate::grid) use aterm_scrollback::{CellAttrs, HyperlinkSpan, Line, Rle};

#[cfg(test)]
pub(in crate::grid) use crate::test_counters::{
    take_extras_clear_ops, take_extras_shift_ops, take_reflow_cell_ops, take_reflow_row_ops,
    take_row_to_line_ops,
};

pub use anchor_lend::{AnchorLend, LendReach};
pub use checkpoint_lines::GridImageCarry;
pub use history_carry::{
    HistoryFence, HistoryFenceBroken, OlderHistory, OlderHistoryClaim, OlderHistoryRefusal,
};
pub use resize_handback::{CanonicalHandbackCarry, GridHandbackCarry, ResizeLaw};
pub(crate) use scroll_convert::ScrolledRowExtras;
pub use scroll_materialize::{MaterializedRow, materialize_from_line, stored_line_cells};
pub use scrollback_access::HistoryLines;
pub use scrollback_offload::{PendingScrollbackReflow, ReflowStep, ReflowedScrollback};
pub use visible_row_view::{CellDataView, VisibleRowView};

/// Convert an i32 to u16 after clamping to non-negative (cursor math).
#[cfg(test)]
#[inline]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=u16::MAX before cast"
)]
const fn clamp_u16(val: i32) -> u16 {
    if val <= 0 {
        0
    } else if val > u16::MAX as i32 {
        u16::MAX
    } else {
        val as u16
    }
}

use state::GridStorage;
pub(in crate::grid) use viewport_row_cache::{HistoryEpoch, ViewportRowCache};

/// Terminal grid with ring-buffered display scrolling.
#[derive(Debug)]
pub struct Grid {
    /// All grid state: rows, cursor, damage, extras, scrollback.
    pub(crate) storage: GridStorage,
    /// Derived state: the memo of materialized history rows a scrolled-back
    /// viewport reads. Rebuildable from `storage` alone, which is why it may be
    /// filled through `&self`; see `viewport_row_cache`.
    pub(in crate::grid) viewport_cache: ViewportRowCache,
}

impl Grid {
    /// Get a row by visible row index.
    #[must_use]
    #[inline]
    pub fn row(&self, visible_row: u16) -> Option<&Row> {
        self.storage.row(visible_row)
    }

    /// Get a mutable row by visible row index.
    #[inline]
    pub fn row_mut(&mut self, visible_row: u16) -> Option<&mut Row> {
        self.storage.row_mut(visible_row)
    }

    /// Get a cell at the given position.
    #[must_use]
    #[inline]
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        self.row(row).and_then(|r| r.get(col))
    }

    /// Get a mutable cell at the given position.
    #[cfg(test)]
    #[inline]
    pub fn cell_mut(&mut self, row: u16, col: u16) -> Option<&mut Cell> {
        self.row_mut(row).and_then(|r| r.get_mut(col))
    }

    /// Whether the cell at (row, col) is a wide-character continuation spacer.
    #[must_use]
    #[inline]
    pub fn is_wide_continuation_at(&self, row: u16, col: u16) -> bool {
        self.row(row)
            .is_some_and(|r| r.is_cell_wide_continuation(col))
    }

    /// Set the BCE (background colour erase) template cell.
    #[inline]
    pub const fn set_cursor_template(&mut self, template: Cell, bg_rgb: Option<[u8; 3]>) {
        self.storage.cursor_state.cursor_template = template;
        self.storage.cursor_state.cursor_template_bg_rgb = bg_rgb;
    }
}
