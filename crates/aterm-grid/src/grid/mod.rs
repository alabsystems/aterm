// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal grid with O(1) display scrolling, O(n × cols) content scrolling.
//!
//! Owns `Grid`, `GridStorage`, `MaterializedRow`, and all production
//! `impl Grid` methods.

mod accessors;
mod buffer_access;
mod checkpoint_lines;
mod construct;
mod content_queries;
mod cursor_ops;
mod erase;
mod invariants;
mod line_ops;
mod pin_methods;
pub mod reflow;
mod scroll;
pub mod scroll_convert;
mod scroll_fill;
pub mod scroll_materialize;
mod scroll_unscroll;
mod scrollback_access;
mod scrollback_offload;
mod scrollback_reflow;
// Disk cold-tier spill (mmap-backed); dropped on wasm (no libc/zstd-sys).
#[cfg(feature = "disk-tier")]
pub mod scrollback_budget;
mod search_content;
pub mod state;
mod tab_ops;
mod visible_row_view;
mod write;
mod write_split;

#[cfg(kani)]
mod proofs_kani;
#[cfg(kani)]
mod proofs_kani_cursor;
#[cfg(kani)]
mod proofs_kani_extras;
#[cfg(kani)]
mod proofs_kani_extras_invariants;
#[cfg(kani)]
mod proofs_kani_ring;
#[cfg(kani)]
mod proofs_kani_scroll;
#[cfg(kani)]
#[path = "shift_coord_arithmetic_tests.rs"]
mod proofs_kani_shift_coord;
#[cfg(kani)]
mod proofs_kani_style;
#[cfg(kani)]
mod proofs_kani_tabs;

#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "testing"))]
#[allow(
    dead_code,
    reason = "test-only grid write helpers; most callers are #[cfg(test)] only (#6799)"
)]
mod write_test_helpers;

// Re-export crate types visible to all grid submodules so moved files
// can use `super::X` paths for frequently-used types.
pub(in crate::grid) use crate::GenerationTracker;
pub(in crate::grid) use crate::row_u16;
pub(in crate::grid) use crate::{Cell, CellFlags, PackedColor, PackedColors};
pub(in crate::grid) use crate::{CellCoord, CellExtra, CellExtras};
pub(in crate::grid) use crate::{ColorType, Cursor, ExtendedStyle, StyleId};
pub(in crate::grid) use crate::{HorizontalMargins, PAGE_SIZE, PageStore, ScrollRegion};
pub(in crate::grid) use crate::{LineSize, Row};

// Kani constants re-exports.
#[cfg(kani)]
pub(in crate::grid) use crate::{KANI_MAX_COLS, KANI_MAX_ROWS};

// Test counter re-exports so grid submodules can use `super::count_*`.
#[cfg(any(test, feature = "testing"))]
pub(in crate::grid) use crate::test_counters::{
    count_reflow_row_op, count_row_to_line_cell, count_row_to_line_op,
    count_scrollback_reflow_sync_lines,
};

// Scrollback re-exports for test files migrated from aterm-core (#6556).
#[cfg(test)]
pub(in crate::grid) use aterm_scrollback::Scrollback;
#[cfg(test)]
pub(in crate::grid) use aterm_scrollback::{CellAttrs, HyperlinkSpan, Line, Rle};

// Test counter take_* re-exports for performance/complexity tests (#6556 Batch 2).
#[cfg(test)]
pub(in crate::grid) use crate::test_counters::{
    take_extras_clear_ops, take_extras_shift_ops, take_reflow_cell_ops, take_reflow_row_ops,
    take_row_to_line_cells, take_row_to_line_ops,
};

pub(crate) use scroll_convert::ScrolledRowExtras;
pub use scroll_materialize::{MaterializedRow, materialize_from_line};
pub use scrollback_offload::{PendingScrollbackReflow, ReflowStep, ReflowedScrollback};
pub use visible_row_view::{CellDataView, VisibleRowView};

/// Convert i32 result to u16 after clamping to non-negative.
///
/// Used for cursor math where we clamp to [0, max] range.
#[cfg(any(test, feature = "fuzz", fuzzing, feature = "testing"))]
#[inline]
fn clamp_u16(val: i32) -> u16 {
    val.max(0).try_into().unwrap_or(u16::MAX)
}

use state::GridStorage;

/// Terminal grid.
///
/// Uses a ring buffer for O(1) scrolling. The `display_offset` determines
/// what portion of the history is shown.
#[derive(Debug)]
pub struct Grid {
    /// All grid state: rows, cursor, damage, extras, styles, scrollback.
    ///
    /// Accessed explicitly as `self.storage` — no `Deref` chain (#6917).
    pub(crate) storage: GridStorage,
}

impl Grid {
    /// Get a row by visible row index.
    #[must_use]
    pub fn row(&self, visible_row: u16) -> Option<&Row> {
        self.storage.row(visible_row)
    }

    /// Get a mutable row by visible row index.
    #[inline]
    pub fn row_mut(&mut self, visible_row: u16) -> Option<&mut Row> {
        self.storage.row_mut(visible_row)
    }

    /// Get a contiguous slice of all cells in a row (#7861).
    ///
    /// Returns the row's backing `&[Cell]` slice in a single ring-buffer
    /// lookup. Callers can iterate the slice directly, avoiding per-cell
    /// bounds checks from `Row::get()`.
    #[must_use]
    #[inline]
    pub fn row_cells_slice(&self, visible_row: u16) -> Option<&[Cell]> {
        self.row(visible_row).map(Row::as_slice)
    }

    /// Get a cell at the given position.
    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        self.row(row).and_then(|r| r.get(col))
    }

    /// Get a mutable cell at the given position.
    pub fn cell_mut(&mut self, row: u16, col: u16) -> Option<&mut Cell> {
        self.row_mut(row).and_then(|r| r.get_mut(col))
    }

    /// Whether the cell at (row, col) is a wide-character continuation spacer.
    ///
    /// Unlike `Cell::is_wide_continuation()` — which false-positives on
    /// DECSCA-protected cells because `PROTECTED` and `WIDE_CONTINUATION`
    /// share bit 10 — this disambiguates via the left neighbor: a true
    /// continuation always immediately follows its `WIDE` main cell.
    #[must_use]
    #[inline]
    pub fn is_wide_continuation_at(&self, row: u16, col: u16) -> bool {
        self.row(row)
            .is_some_and(|r| r.is_cell_wide_continuation(col))
    }

    /// Get a mutable row and its effective column count in a single ring-buffer lookup.
    #[inline]
    pub fn row_mut_with_effective_cols(&mut self, visible_row: u16) -> Option<(&mut Row, u16)> {
        self.storage.row_mut_with_effective_cols(visible_row)
    }

    /// Set the BCE (Background Color Erase) cursor template cell.
    ///
    /// The cursor template is used by erase operations (ED, EL, ECH, IL, DL,
    /// scroll) to fill erased cells with the current SGR background color
    /// instead of default. Call this whenever the SGR background changes.
    ///
    /// `bg_rgb` must be `Some([r, g, b])` when the current SGR background is
    /// truecolor (24-bit RGB). Erase operations write this value into the
    /// `RgbColorRing` for every filled cell so the renderer can resolve the
    /// actual color. Without it, cells get the "RGB mode" flag but no value.
    ///
    /// Per VT420/xterm BCE specification (#7522). Fixes #7685.
    #[inline]
    pub fn set_cursor_template(&mut self, template: Cell, bg_rgb: Option<[u8; 3]>) {
        self.storage.cursor_template = template;
        self.storage.cursor_template_bg_rgb = bg_rgb;
    }

    /// Get the current BCE cursor template cell.
    #[must_use]
    #[inline]
    pub fn cursor_template(&self) -> Cell {
        self.storage.cursor_template
    }
}
