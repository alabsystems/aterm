// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Terminal grid facade.
//!
//! `Grid`, `GridStorage`, `MaterializedRow`, and all production `impl Grid`
//! methods now live in `aterm-grid`. This module re-exports them and provides
//! backward-compatible import paths for leaf types and FFI/test shims.

// ============================================================================
// Leaf-type facade modules (explicit exports, Part of #6781)
// ============================================================================

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
    // Preserve the testing-only module-path facade surface from aterm-grid.
    #[cfg(test)]
    pub use aterm_grid::damage::{DamageRect, MergedDamageIterator};
}
/// Page-backed memory storage. Re-exported from `aterm-grid`.
pub mod page {
    pub use aterm_grid::{PAGE_SIZE, PageStore};
}
/// Row storage for terminal grid. Re-exported from `aterm-grid`.
pub mod row {
    pub use aterm_grid::row::{LineSize, Row, RowFlags};
}
/// Style deduplication and interning. Re-exported from `aterm-grid`.
pub mod style {
    #[cfg(test)]
    pub use aterm_grid::style::ExtendedStyleInfo;
    pub use aterm_grid::style::{
        Color, ColorType, ExtendedStyle, Style, StyleAttrs, StyleId, StyleTable,
    };
}
/// Cell extras for rarely-used attributes. Re-exported from `aterm-grid`.
pub mod extra {
    #[cfg(test)]
    pub use aterm_grid::extra::is_combining_mark;
    pub use aterm_grid::extra::{
        CellCoord, CellExtra, ImageData, ImageFormat, ImageRef, KittyPlaceholderData, UniformExtras,
    };
    pub use aterm_grid::extra_collection::CellExtras;
}
/// Cell extras collection. Re-exported from `aterm-grid`.
pub mod extra_collection {
    pub use aterm_grid::extra_collection::CellExtras;
}

// ============================================================================
// Grid ownership — re-exported from aterm-grid
// ============================================================================

pub use aterm_grid::grid::Grid;
pub use aterm_grid::grid::{MaterializedRow, materialize_from_line};

// The scrollback-reflow offload job types. These already appear in
// `Terminal::resize_offloading_scrollback` / `Terminal::finish_resize_offload`'s
// PUBLIC signatures (as `aterm_grid::…`), so any aterm-core consumer that must
// HOLD the job across calls — the wasm modules' cooperative (thread-free)
// offload stashes it in module state between host turns — needs to NAME the
// type. Re-exported here so that doesn't force a direct aterm-grid dependency.
pub use aterm_grid::{PendingScrollbackReflow, ReflowStep, ReflowedScrollback};

// ============================================================================
// Modules that remain in aterm-core
// ============================================================================

/// FFI bridge for grid operations.
pub(crate) mod ffi_bridge;

#[cfg(test)]
#[path = "../../test_support/grid/tests/mod.rs"]
mod tests;

// ============================================================================
// Backward-compatible re-exports
// ============================================================================

pub use cell::{Cell, CellFlags, PackedColor, PackedColors};
pub use damage::{Damage, DamagedRowIterator, LineDamageBounds, RowDamageBounds};
pub use extra::{CellCoord, CellExtra, CellExtras, KittyPlaceholderData, UniformExtras};
// `crate::grid::PAGE_SIZE` is the flat re-export consumed by the FFI safe-helper
// layer (ffi_bridge/safe_helpers.rs compile-time size guard); in-crate code
// reaches it via `grid::page::PAGE_SIZE`.
#[allow(
    unused_imports,
    reason = "flat re-export consumed by the FFI/verification layer"
)]
pub(crate) use page::PAGE_SIZE;
pub use page::PageStore;
pub use row::{LineSize, Row, RowFlags};
pub use style::{Color, ColorType, ExtendedStyle, Style, StyleAttrs, StyleId, StyleTable};

// Batch 2A: cursor and region types re-exported from aterm-grid.
pub use aterm_grid::{CurrentStyle, Cursor, SavedCursor, SavedCursorState};

// Grid dimension ingress bounds (§5.8): constructors and resize clamp to these.
pub use aterm_grid::{MAX_GRID_COLS, MAX_GRID_ROWS};

// ----------------------------------------------------------------------------
// Row index conversion helpers
// ----------------------------------------------------------------------------

pub(crate) use aterm_grid::row_u16;
