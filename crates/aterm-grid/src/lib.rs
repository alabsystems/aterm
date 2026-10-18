// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

// Production unwrap() is forbidden; tests opt out at the crate root so a
// missing per-module allow cannot slip past clippy.
#![warn(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! Terminal grid model: `Grid`, `GridStorage`, `MaterializedRow`, cells, rows,
//! damage tracking and page storage.

pub mod cell;
pub(crate) mod cell_colors;
pub(crate) mod cell_flags;
pub mod cursor;
pub mod damage;
pub mod extra;
pub mod extra_collection;
mod extra_collection_shifts;
/// Page-backed storage internals. Exposed under `debug_assertions` for
/// downstream property tests; production uses [`Row`] and
/// [`PageStore`] (re-exported at the crate root).
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
pub use grid::{CellDataView, HistoryLines, VisibleRowView};
pub use grid::{
    HistoryFence, HistoryFenceBroken, OlderHistory, OlderHistoryClaim, OlderHistoryRefusal,
};
pub use grid::{MaterializedRow, materialize_from_line, stored_line_cells};
pub use grid::{PendingScrollbackReflow, ReflowStep, ReflowedScrollback};

// Re-export primary types at crate root for convenience.
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

// Page-level constants and aliases other crates use.
pub use page::PAGE_SIZE;
pub use page::PageId;

// Cursor, scroll region, and grid state types.
pub use cursor::{Cursor, SavedCursor};
pub use scroll_region::{HorizontalMargins, ScrollRegion};
pub use state::{
    AbsoluteRowUpdate, BandSet, GridCursorState, GridPresentationState, MAX_SELECTION_DAMAGE_BANDS,
    OutputOrigin, ROW_BAND_MOVES_PER_BATCH, ResizeShape, RowBandMove, RowBandMoves,
    SelectionDamage,
};

// Terminal style types shared with checkpoint system.
pub mod terminal_style;
pub use terminal_style::{CurrentStyle, SavedCursorState};

/// Convert usize row index to u16 (saturating at `u16::MAX`).
#[must_use]
#[inline]
pub fn row_u16(idx: usize) -> u16 {
    idx.try_into().unwrap_or(u16::MAX)
}

/// Maximum visible rows a [`Grid`] will hold. Constructors and
/// [`Grid::resize`] clamp both dimensions to `1..=MAX_GRID_ROWS/COLS`, so a
/// hostile `u16::MAX × u16::MAX` resize cannot allocate billions of cells.
pub const MAX_GRID_ROWS: u16 = 4096;

/// Maximum columns a [`Grid`] will hold; see [`MAX_GRID_ROWS`].
pub const MAX_GRID_COLS: u16 = 4096;

/// Whether column `col` holds a tab stop in a fresh grid: every eighth column
/// from 8, the VT100's power-on stops. Hosts that carry a grid's stops as a
/// difference from the default (`aterm-gui`'s keeper META) read this too.
#[must_use]
#[inline]
pub const fn is_default_tab_stop(col: usize) -> bool {
    col > 0 && col.is_multiple_of(8)
}
