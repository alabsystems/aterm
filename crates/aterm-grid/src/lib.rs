// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::all)]
// F11-4 (#7941): production unwrap() is forbidden; tests opt out uniformly at the
// crate root so a missing per-module allow can't silently slip past clippy.
#![deny(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! Terminal grid model: Grid, cells, rows, styles, damage tracking, page storage.
//!
//! This crate owns the terminal grid model including `Grid`, `GridStorage`,
//! `MaterializedRow`, and all production `impl Grid` methods. Leaf types
//! (Cell, Row, Style, Damage, etc.) and the Grid itself live here.

pub mod cell;
pub(crate) mod cell_colors;
pub(crate) mod cell_flags;
pub mod cursor;
pub mod damage;
pub mod extra;
pub mod extra_collection;
mod extra_collection_shifts;
/// Page-backed storage internals.
///
/// Crate-internal in production builds. Accessible to downstream crate tests
/// via `feature = "testing"` so `PageStore::alloc_slice` and `PageSlice`
/// remain available for property tests and Kani scaffolding.
/// Production consumers use [`Row`] and [`PageStore`] (re-exported at crate
/// root) as their safe API boundary (#5573).
#[cfg(any(test, kani, feature = "testing"))]
pub mod page;
#[cfg(not(any(test, kani, feature = "testing")))]
pub(crate) mod page;
pub mod pin;
pub mod row;
pub mod scroll_region;
pub mod state;
pub mod style;

pub mod grid;

#[cfg(all(test, not(feature = "testing")))]
pub(crate) mod test_counters;
#[cfg(feature = "testing")]
pub mod test_counters;

#[cfg(test)]
mod mem_measure_tests;

// TRUST_NATIVE_TLA §4 (Phase 4): the kani-half of the unified verifier ledger.
//
// UN-GATED w.r.t. `cfg(kani)` (the ring-buffer harnesses in `grid/proofs_kani_ring.rs`
// are `#[cfg(kani)]` and dormant under stock cargo). The `proof_anchor!` REGISTRATIONS
// must NOT share that gate — they name the harnesses by string and are gated ONLY by the
// `spec-anchors` feature (or `test`), so they register in normal/test builds. See the
// module doc for the mapping.
#[cfg(any(test, feature = "spec-anchors"))]
mod spec_proof_anchors;

// Re-export Grid and related types at crate root.
pub use grid::Grid;
pub use grid::scroll_convert::{scrollback_text_only, set_scrollback_text_only};
pub use grid::{CellDataView, VisibleRowView};
pub use grid::{MaterializedRow, materialize_from_line};
pub use grid::{PendingScrollbackReflow, ReflowStep, ReflowedScrollback};

// Re-export scrollback budget types. Gated with the disk cold-tier (wasm drops it).
#[cfg(feature = "disk-tier")]
pub use grid::scrollback_budget::{BudgetError, ScrollbackBudget, ScrollbackMemoryStats};

// Re-export primary types at crate root for convenience.
pub use cell::{Cell, CellFlags, PackedColor, PackedColors};
pub use damage::{
    Damage, DamageBoundsIterator, DamagedRowIterator, LineDamageBounds, RowDamageBounds,
};
#[cfg(any(test, kani, feature = "testing"))]
pub use extra::is_combining_mark;
pub use extra::{
    CellCoord, CellExtra, CellExtras, ImageData, ImageFormat, ImageRef, KittyPlaceholderData,
    UniformExtras,
};
pub use page::PageStore;
pub use pin::GenerationTracker;
pub use row::{LineSize, Row, RowFlags};
pub use style::{Color, ColorType, ExtendedStyle, Style, StyleAttrs, StyleId, StyleTable};

// Re-export page-level constants and harmless aliases needed by other crates.
pub use page::PAGE_SIZE;
pub use page::PageId;
#[cfg(any(test, feature = "testing", kani))]
pub use pin::{Generation, Pin, PinnedRange};

// Batch 2A: cursor, scroll region, and grid state types.
pub use cursor::{Cursor, SavedCursor};
pub use scroll_region::{HorizontalMargins, ScrollRegion};
pub use state::{
    AbsoluteRowUpdate, BandSet, GridCursorState, GridPresentationState, MAX_SELECTION_DAMAGE_BANDS,
    OutputOrigin, SelectionDamage,
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

/// Maximum visible rows a [`Grid`] will hold.
///
/// Ingress bound (design §5.8): a hostile `u16::MAX × u16::MAX` resize asks
/// for ~4.3 billion cells, an allocation bomb. Constructors and
/// [`Grid::resize`] clamp both dimensions to `1..=MAX_GRID_ROWS/COLS`.
pub const MAX_GRID_ROWS: u16 = 4096;

/// Maximum columns a [`Grid`] will hold; see [`MAX_GRID_ROWS`].
pub const MAX_GRID_COLS: u16 = 4096;

/// Maximum rows for Kani grid stubs.
#[cfg(kani)]
pub const KANI_MAX_ROWS: u16 = 8;

/// Maximum columns for Kani grid stubs.
#[cfg(kani)]
pub const KANI_MAX_COLS: u16 = 16;
