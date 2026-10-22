// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Terminal grid facade: re-exports `aterm-grid`'s `Grid` and its leaf types under the
//! module paths aterm-core callers use.

/// Packed cell representation (8 bytes). Re-exported from `aterm-grid`.
pub mod cell {
    pub use aterm_grid::cell::Cell;
    pub use aterm_grid::{CellFlags, PackedColor, PackedColors};
}
/// Packed color types. Re-exported from `aterm-grid`.
pub mod cell_colors {
    pub use aterm_grid::{PackedColor, PackedColors};
}
/// Cell attribute flags. Re-exported from `aterm-grid`.
pub mod cell_flags {
    pub use aterm_grid::CellFlags;
}
/// Damage tracking for efficient rendering. Re-exported from `aterm-grid`.
pub mod damage {
    pub use aterm_grid::damage::{
        BitsetRowIterator, Damage, DamageBoundsIterator, DamageTracker, DamagedRowIterator,
        LineDamageBounds, RowDamageBounds,
    };
}
/// Page-backed memory storage. Re-exported from `aterm-grid`.
pub mod page {
    pub use aterm_grid::{PAGE_SIZE, PageStore};
}
/// Row storage for terminal grid. Re-exported from `aterm-grid`.
pub mod row {
    pub use aterm_grid::row::{LineSize, Row, RowFlags};
}
/// Cell extras for rarely-used attributes. Re-exported from `aterm-grid`.
pub mod extra {
    #[cfg(test)]
    pub use aterm_grid::extra::is_combining_mark;
    pub use aterm_grid::extra::{
        CellCoord, CellExtra, ImageData, ImageFormat, ImageRef, ImageScaling, KittyPlacementTag,
        SourceRect, UniformExtras,
    };
    pub use aterm_grid::extra_collection::CellExtras;
}
/// Cell extras collection. Re-exported from `aterm-grid`.
pub mod extra_collection {
    pub use aterm_grid::extra_collection::CellExtras;
}

pub use aterm_grid::grid::Grid;
pub use aterm_grid::grid::{MaterializedRow, materialize_from_line, stored_line_cells};

// In `Terminal::resize_offloading_scrollback`'s signature: hosts holding the job across calls
// (the wasm modules' cooperative offload) name them without an aterm-grid dependency.
pub use aterm_grid::{PendingScrollbackReflow, ReflowStep, ReflowedScrollback};
// The host-repaint policy `Terminal::resize_with_policy` takes; the GUI picks it per session
// from the PTY backend (ConPTY on Windows).
pub use aterm_grid::grid::reflow::ResizePolicy;
// The resize law a recording names (`aterm_resize_undo`), which `cast drift`
// replays it under (`Terminal::set_resize_law`).
pub use aterm_grid::ResizeLaw;

// The seamless update's history carry (`Terminal::history_fence` and the import), named from
// here by the host's handoff (`aterm-gui`).
pub use aterm_grid::{
    HistoryFence, HistoryFenceBroken, OlderHistory, OlderHistoryClaim, OlderHistoryRefusal,
};

pub use cell::{Cell, CellFlags, PackedColor, PackedColors};
pub use damage::{Damage, DamagedRowIterator, LineDamageBounds, RowDamageBounds};
pub use extra::{CellCoord, CellExtra, CellExtras, UniformExtras};
pub use page::PageStore;
pub use row::{LineSize, Row, RowFlags};

pub use aterm_grid::{CurrentStyle, Cursor, SavedCursor, SavedCursorState};

// Grid dimension bounds: constructors and resize clamp to these.
pub use aterm_grid::{MAX_GRID_COLS, MAX_GRID_ROWS};
// The every-8 default tab stop, so a host carrying stops compactly uses the grid's own rule.
pub use aterm_grid::is_default_tab_stop;

pub(crate) use aterm_grid::row_u16;
