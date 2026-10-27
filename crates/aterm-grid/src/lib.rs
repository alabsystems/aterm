// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#![warn(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]
#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

//! Terminal grid model: [`Grid`], [`MaterializedRow`], cells, rows, damage and page storage.

pub mod cell;
pub(crate) mod cell_colors;
pub(crate) mod cell_flags;
pub mod cursor;
pub mod damage;
pub mod extra;
pub mod extra_collection;
mod extra_collection_shifts;
/// Page-backed storage internals ([`PageStore`]; [`Row`] is their safe boundary).
#[cfg(any(test, debug_assertions))]
pub mod page;
#[cfg(not(any(test, debug_assertions)))]
pub(crate) mod page;
pub mod row;
pub mod scroll_region;
pub mod state;

pub mod grid;

#[cfg(all(test, not(debug_assertions)))]
pub(crate) mod test_counters;
#[cfg(debug_assertions)]
pub mod test_counters;

pub use grid::Grid;
pub use grid::scroll_convert::{scrollback_text_only, set_scrollback_text_only};
pub use grid::{AnchorLend, LendReach};
pub use grid::{CanonicalHandbackCarry, GridHandbackCarry, ResizeLaw};
pub use grid::{CellDataView, HistoryLines, VisibleRowView};
pub use grid::{
    HistoryFence, HistoryFenceBroken, OlderHistory, OlderHistoryClaim, OlderHistoryRefusal,
};
pub use grid::{MaterializedRow, materialize_from_line, stored_line_cells};
pub use grid::{PendingScrollbackReflow, ReflowStep, ReflowedScrollback};

pub use cell::{Cell, CellFlags, PackedColor, PackedColors};
pub use damage::{
    Damage, DamageBoundsIterator, DamagedRowIterator, LineDamageBounds, RowDamageBounds,
};
#[cfg(any(test, debug_assertions))]
pub use extra::is_combining_mark;
pub use extra::{
    CellCoord, CellExtra, CellExtras, ImageData, ImageFormat, ImageRef, ImageScaling,
    KittyPlacementTag, MAX_IMAGE_LAYERS, SourceRect, UniformExtras, underline_color_sgr,
};
pub use page::PageStore;
pub use row::{LineSize, Row, RowFlags};

pub use page::PAGE_SIZE;
pub use page::PageId;

pub use cursor::{Cursor, SavedCursor};
pub use scroll_region::{HorizontalMargins, ScrollRegion};
#[cfg(any(test, debug_assertions))]
pub use state::with_knob;
pub use state::{
    AbsoluteRowUpdate, Anchor, AnchorId, AnchorRole, AnchorSet, BandSet, FenceKnob,
    GridCursorState, GridPresentationState, MAX_ANCHORS, MAX_SELECTION_DAMAGE_BANDS, OutputOrigin,
    ROW_BAND_MOVES_PER_BATCH, ResizeShape, RowBandMove, RowBandMoves, SelectionDamage, knob,
};

pub mod terminal_style;
pub use terminal_style::{CurrentStyle, SavedCursorState};

/// Convert usize row index to u16 (saturating at `u16::MAX`).
#[must_use]
#[inline]
#[allow(
    clippy::cast_possible_truncation,
    reason = "guarded by idx <= u16::MAX as usize before cast"
)]
pub const fn row_u16(idx: usize) -> u16 {
    if idx > u16::MAX as usize {
        u16::MAX
    } else {
        idx as u16
    }
}

/// Maximum visible rows a [`Grid`] will hold ([`Grid::resize`]).
pub const MAX_GRID_ROWS: u16 = 4096;

/// Maximum columns a [`Grid`] will hold ([`MAX_GRID_ROWS`]).
pub const MAX_GRID_COLS: u16 = 4096;

/// Whether column `col` holds a tab stop in a fresh grid.
#[must_use]
#[inline]
pub const fn is_default_tab_stop(col: usize) -> bool {
    col > 0 && col.is_multiple_of(8)
}
