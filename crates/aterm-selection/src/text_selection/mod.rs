// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Mouse-based text selection state machine.
//!
//! This module implements the text selection system per the TLA+ spec in `tla/Selection.tla`.
//!
//! Selection lifecycle:
//! - None -> InProgress (start selection on mouse down)
//! - InProgress -> Complete (finish selection on mouse up)
//! - Complete -> InProgress (extend selection with shift-click)
//! - InProgress/Complete -> None (clear selection)
//!
//! Selection types:
//! - Simple: Character-by-character selection
//! - Block: Rectangular selection (column mode)
//! - Semantic: Word/URL selection (double-click)
//! - Lines: Full line selection (triple-click)

use std::cmp::Ordering;

pub use aterm_types::selection::{SelectionSide, SelectionType};

/// Side-aware projection of a selection into concrete grid coordinates.
///
/// This is the output of [`TextSelection::project_range`] and provides exactly
/// the information a consumer needs to render or hit-test a selection without
/// access to the private `SelectionAnchor::side` field.
///
/// Column values are already adjusted for side: a `Right`-sided start anchor
/// shifts `start_col` forward by one, and a `Left`-sided end anchor shifts
/// `end_col` backward by one. For `Lines` selections, columns span the full
/// line width (0 to `last_col`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionProjection {
    /// Start row (can be negative for scrollback).
    pub start_row: i32,
    /// Start column (0-indexed, side-adjusted).
    pub start_col: u16,
    /// End row (can be negative for scrollback).
    pub end_row: i32,
    /// End column (0-indexed, side-adjusted).
    pub end_col: u16,
    /// Whether this is a block (rectangular) selection.
    pub is_block: bool,
}

#[cfg(kani)]
mod anchor_proofs;
#[cfg(kani)]
mod state_machine_proofs;

#[cfg(test)]
mod tests;

/// Selection state enum matching the TLA+ spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionState {
    /// No selection active.
    #[default]
    None,
    /// Selection in progress (mouse button held down).
    InProgress,
    /// Selection complete (mouse button released).
    Complete,
}

/// A selection anchor point.
///
/// An anchor marks one end of a selection. It includes:
/// - Row: Can be negative for scrollback
/// - Column: 0-indexed
/// - Side: Which side of the cell
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SelectionAnchor {
    /// Row (can be negative for scrollback).
    pub row: i32,
    /// Column (0-indexed).
    pub col: u16,
    /// Which side of the cell.
    pub(crate) side: SelectionSide,
}

impl SelectionAnchor {
    /// Create a new anchor at the given position.
    #[inline]
    pub(super) const fn new(row: i32, col: u16, side: SelectionSide) -> Self {
        Self { row, col, side }
    }

    /// Create an anchor at the left side of a cell.
    #[inline]
    #[cfg(kani)]
    pub(super) const fn left(row: i32, col: u16) -> Self {
        Self::new(row, col, SelectionSide::Left)
    }

    /// Create an anchor at the right side of a cell.
    #[inline]
    #[cfg(kani)]
    pub(super) const fn right(row: i32, col: u16) -> Self {
        Self::new(row, col, SelectionSide::Right)
    }
}

impl PartialOrd for SelectionAnchor {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SelectionAnchor {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.row.cmp(&other.row) {
            Ordering::Equal => match self.col.cmp(&other.col) {
                Ordering::Equal => self.side.cmp(&other.side),
                other => other,
            },
            other => other,
        }
    }
}

/// Text selection state.
///
/// This is a state machine implementing the TLA+ spec in `tla/Selection.tla`.
///
/// `PartialEq`/`Eq` exist so the renderer can detect a selection change between
/// frames (the damage-tracking fast path falls back to a full render whenever
/// the selection differs); two selections are equal iff all four fields match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextSelection {
    /// Current selection state.
    state: SelectionState,
    /// Selection type (only valid when state != None).
    selection_type: SelectionType,
    /// Start anchor (set on mouse down).
    start: SelectionAnchor,
    /// End anchor (updated on mouse move).
    end: SelectionAnchor,
}

impl TextSelection {
    /// Create a new empty selection.
    #[inline]
    pub const fn new() -> Self {
        Self {
            state: SelectionState::None,
            selection_type: SelectionType::Simple,
            start: SelectionAnchor::new(0, 0, SelectionSide::Left),
            end: SelectionAnchor::new(0, 0, SelectionSide::Left),
        }
    }

    /// Get the current selection state.
    #[inline]
    pub const fn state(&self) -> SelectionState {
        self.state
    }

    /// Get the selection type.
    #[inline]
    pub const fn selection_type(&self) -> SelectionType {
        self.selection_type
    }

    /// Check if there is an active selection.
    #[inline]
    pub const fn has_selection(&self) -> bool {
        !matches!(self.state, SelectionState::None)
    }

    /// Check if selection is complete (mouse button released).
    #[cfg(any(test, kani))]
    #[inline]
    pub(crate) const fn is_complete(&self) -> bool {
        matches!(self.state, SelectionState::Complete)
    }

    /// Check if selection is in progress (mouse button held).
    #[cfg(any(test, kani))]
    #[inline]
    pub(crate) const fn is_in_progress(&self) -> bool {
        matches!(self.state, SelectionState::InProgress)
    }

    /// Check if the selection is empty (no active selection or start equals end).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.state == SelectionState::None || self.start == self.end
    }

    /// Get the raw (un-normalized) start anchor.
    ///
    /// Returns the anchor set by `start_selection`. The `side` field is
    /// `pub(crate)`, so external callers can read `row` and `col` only.
    #[inline]
    pub const fn start(&self) -> SelectionAnchor {
        self.start
    }

    /// Get the raw (un-normalized) end anchor.
    ///
    /// Returns the anchor last updated by `update_selection`. The `side`
    /// field is `pub(crate)`, so external callers can read `row` and `col` only.
    #[inline]
    pub const fn end(&self) -> SelectionAnchor {
        self.end
    }

    /// Get the normalized start (the anchor that comes first).
    ///
    /// For block selection, returns the top-left corner.
    #[inline]
    pub fn normalized_start(&self) -> SelectionAnchor {
        if self.selection_type == SelectionType::Block {
            // For block selection, return top-left
            SelectionAnchor::new(
                self.start.row.min(self.end.row),
                self.start.col.min(self.end.col),
                SelectionSide::Left,
            )
        } else if self.start <= self.end {
            self.start
        } else {
            self.end
        }
    }

    /// Get the normalized end (the anchor that comes last).
    ///
    /// For block selection, returns the bottom-right corner.
    #[inline]
    pub fn normalized_end(&self) -> SelectionAnchor {
        if self.selection_type == SelectionType::Block {
            // For block selection, return bottom-right
            SelectionAnchor::new(
                self.start.row.max(self.end.row),
                self.start.col.max(self.end.col),
                SelectionSide::Right,
            )
        } else if self.start <= self.end {
            self.end
        } else {
            self.start
        }
    }

    /// Get normalized selection bounds as primitive coordinates.
    ///
    /// Returns `(start_row, start_col, end_row, end_col)` where start is the
    /// top-left endpoint for block selection and the earliest endpoint for
    /// linear selections.
    #[inline]
    pub fn normalized_bounds(&self) -> (i32, u16, i32, u16) {
        let start = self.normalized_start();
        let end = self.normalized_end();
        (start.row, start.col, end.row, end.col)
    }

    /// Apply sub-cell side adjustment to a normalized anchor pair.
    ///
    /// Returns `(start_row, start_col, end_row, end_col)`. A `Right`-sided start
    /// shifts `start_col` forward by 1; a `Left`-sided end shifts `end_col`
    /// backward by 1. A `Left`-sided end at col 0 means "stop before column 0",
    /// which excludes the whole end row — retreat to the previous row with
    /// `end_col = u16::MAX`. Shared by `side_adjusted_bounds`, `contains`, and
    /// `project_range` so all sites agree on the selected region.
    fn apply_side_adjustment(ns: SelectionAnchor, ne: SelectionAnchor) -> (i32, u16, i32, u16) {
        let start_col = if ns.side == SelectionSide::Right {
            ns.col.saturating_add(1)
        } else {
            ns.col
        };
        let (end_row, end_col) = if ne.side == SelectionSide::Left {
            if ne.col > 0 {
                (ne.row, ne.col - 1)
            } else {
                (ne.row - 1, u16::MAX)
            }
        } else {
            (ne.row, ne.col)
        };
        (ns.row, start_col, end_row, end_col)
    }

    /// Get side-adjusted selection bounds matching the visual highlight.
    ///
    /// Returns `Some((start_row, start_col, end_row, end_col))` where columns
    /// have been adjusted for sub-cell side positioning. A `Right`-sided start
    /// shifts `start_col` forward by 1; a `Left`-sided end shifts `end_col`
    /// backward by 1. This matches the column range used by [`Self::contains`]
    /// and [`Self::project_range`].
    ///
    /// Returns `None` if no selection is active or if the selection is empty
    /// after side adjustment.
    pub fn side_adjusted_bounds(&self) -> Option<(i32, u16, i32, u16)> {
        if self.state == SelectionState::None {
            return None;
        }

        let ns = self.normalized_start();
        let ne = self.normalized_end();

        // Side adjustment: same logic as contains() and project_range().
        let (start_row, start_col, end_row, end_col) = Self::apply_side_adjustment(ns, ne);

        // After adjustment, start may be past end (empty selection).
        if start_row > end_row {
            return None;
        }
        if start_row == end_row && start_col > end_col {
            return None;
        }

        Some((start_row, start_col, end_row, end_col))
    }

    /// Start a new selection.
    ///
    /// This clears any existing selection and begins a new one at the given position.
    pub fn start_selection(
        &mut self,
        row: i32,
        col: u16,
        side: SelectionSide,
        selection_type: SelectionType,
    ) {
        self.state = SelectionState::InProgress;
        self.selection_type = selection_type;
        self.start = SelectionAnchor::new(row, col, side);
        self.end = SelectionAnchor::new(row, col, side);
    }

    /// Update the selection endpoint (during mouse drag).
    ///
    /// Only works when selection is in progress.
    pub fn update_selection(&mut self, row: i32, col: u16, side: SelectionSide) {
        if self.state == SelectionState::InProgress {
            self.end = SelectionAnchor::new(row, col, side);
        }
    }

    /// Complete the selection (mouse button released).
    pub fn complete_selection(&mut self) {
        if self.state == SelectionState::InProgress {
            self.state = SelectionState::Complete;
        }
    }

    /// Clear the selection.
    pub fn clear(&mut self) {
        self.state = SelectionState::None;
        // Keep anchors for debugging but they're invalid now
    }

    /// Extend an existing complete selection.
    ///
    /// This is used for shift-click to extend selection.
    /// Moves the end anchor to the new position and re-enters `InProgress`
    /// state so that `update_selection` / `complete_selection` can refine
    /// the endpoint further.
    pub fn extend_selection(&mut self, row: i32, col: u16, side: SelectionSide) {
        if self.state == SelectionState::Complete {
            let moving_anchor = SelectionAnchor::new(row, col, side);
            if self.selection_type != SelectionType::Block {
                let fixed_end_anchor =
                    SelectionAnchor::new(self.start.row, self.start.col, SelectionSide::Right);
                self.start.side = if moving_anchor < fixed_end_anchor {
                    SelectionSide::Right
                } else {
                    SelectionSide::Left
                };
            }
            self.end = moving_anchor;
            self.state = SelectionState::InProgress;
        }
    }

    /// Adjust selection for scroll.
    ///
    /// When the terminal scrolls, selection coordinates need to be updated.
    /// Returns false if selection scrolled entirely off-screen and was cleared.
    ///
    /// Selection rows are live-screen coordinates where scrollback is negative
    /// (e.g. a selection made while scrolled back `D` lines has rows near `-D`).
    /// The valid coordinate range therefore spans from `-floor` (the deepest
    /// retained-history row, where `floor` is the scrollback depth) up to the
    /// live bottom at `max_rows`. Deriving the lower bound from the visible
    /// screen height instead of the scrollback floor would wrongly clear
    /// legitimate scrollback selections on ordinary output (#4056 / SCR-1 view
    /// pinning keeps that content on screen), so the caller threads the actual
    /// post-scroll scrollback floor in.
    pub fn adjust_for_scroll(&mut self, delta: i32, max_rows: i32, floor: i32) -> bool {
        if self.state == SelectionState::None {
            return true;
        }

        // Guard against nonsensical max_rows from FFI callers (#7541).
        // max_rows <= 0 would make the upper bound meaningless.
        if max_rows <= 0 {
            self.clear();
            return false;
        }

        // Use saturating_sub to avoid overflow when delta = i32::MAX
        // (region scroll sentinel). The bounds check below handles the result.
        let new_start_row = self.start.row.saturating_sub(delta);
        let new_end_row = self.end.row.saturating_sub(delta);

        // Check if selection is still within the valid coordinate range.
        // Lower bound is the retained-history floor (-floor), not the visible
        // height. floor is a non-negative line count (the caller derives it from
        // a usize), so negation is well-defined; saturating_neg is used
        // defensively. The region-scroll clear still fires because
        // saturating_sub(i32::MAX) lands near i32::MIN < -floor for any
        // realistic scrollback depth.
        let min_row = floor.saturating_neg();
        let max_row = max_rows;

        if new_start_row < min_row
            || new_start_row > max_row
            || new_end_row < min_row
            || new_end_row > max_row
        {
            // Selection scrolled off - clear it
            self.clear();
            return false;
        }

        self.start.row = new_start_row;
        self.end.row = new_end_row;
        true
    }

    /// Translate both anchor rows into a presentation coordinate space.
    ///
    /// This is for composition-only offsets such as prepending window chrome or
    /// placing a pane inside a larger frame. Unlike [`Self::adjust_for_scroll`],
    /// it does not apply terminal-history bounds or clear the selection: state,
    /// selection type, columns, and anchor sides are preserved. Each row uses
    /// saturating addition so even an untrusted or degenerate offset cannot
    /// overflow.
    pub fn translate_rows_for_presentation(&mut self, delta: i32) {
        self.start.row = self.start.row.saturating_add(delta);
        self.end.row = self.end.row.saturating_add(delta);
    }

    /// Re-anchor selection endpoints across a logical row insertion before a
    /// protected footer.
    ///
    /// `boundary_row` is the first pre-insertion row whose content stays fixed
    /// on screen (the footer). Rows before it move toward scrollback by
    /// `inserted`; rows at or after it retain their terminal-relative row. This
    /// is the piecewise counterpart of [`Self::adjust_for_scroll`]: a single
    /// uniform delta would either detach the footer endpoint or detach the
    /// archived endpoint.
    ///
    /// Returns `false` and clears the selection if either remapped endpoint is
    /// outside the retained coordinate range. Selection state, type, columns,
    /// and sub-cell sides are otherwise preserved exactly.
    pub fn adjust_for_row_splice(
        &mut self,
        boundary_row: i32,
        inserted: i32,
        max_rows: i32,
        floor: i32,
    ) -> bool {
        if self.state == SelectionState::None {
            return true;
        }
        if max_rows <= 0 || inserted < 0 {
            self.clear();
            return false;
        }

        let remap = |row: i32| {
            if row < boundary_row {
                row.saturating_sub(inserted)
            } else {
                row
            }
        };
        let new_start_row = remap(self.start.row);
        let new_end_row = remap(self.end.row);
        let min_row = floor.saturating_neg();
        if new_start_row < min_row
            || new_start_row > max_rows
            || new_end_row < min_row
            || new_end_row > max_rows
        {
            self.clear();
            return false;
        }

        self.start.row = new_start_row;
        self.end.row = new_end_row;
        true
    }

    /// Check if a cell is within the selection, accounting for wide characters.
    ///
    /// For block (rectangular) selection, wide characters that straddle the
    /// selection boundary must be snapped to whole-character boundaries:
    /// - If `is_wide` is true (cell at `col` is a double-width character start),
    ///   the cell is selected if either column `col` or `col + 1` (the
    ///   continuation cell) falls within the block bounds.
    /// - If `is_wide_continuation` is true (cell at `col` is the right half of
    ///   a double-width character), the cell is selected when either its own
    ///   column or the preceding lead (`col - 1`) falls within the block bounds.
    ///
    /// For non-block selections, this behaves identically to [`Self::contains`].
    pub fn contains_cell(
        &self,
        row: i32,
        col: u16,
        is_wide: bool,
        is_wide_continuation: bool,
    ) -> bool {
        if self.selection_type == SelectionType::Block {
            if is_wide_continuation {
                return self.contains(row, col)
                    || (col > 0 && self.contains(row, col.saturating_sub(1)));
            }
            if is_wide {
                return self.contains(row, col) || self.contains(row, col.saturating_add(1));
            }
        }
        self.contains(row, col)
    }

    /// Check if a cell is within the selection.
    ///
    /// Returns true if the cell at (row, col) is selected.
    /// Applies the same side adjustment as [`project_range`] so that both
    /// methods agree on the selected region.
    ///
    /// Note: this method does not account for wide (CJK) character boundaries.
    /// For block selection with wide characters, use [`Self::contains_cell`].
    pub fn contains(&self, row: i32, col: u16) -> bool {
        if self.state == SelectionState::None {
            return false;
        }

        let ns = self.normalized_start();
        let ne = self.normalized_end();

        match self.selection_type {
            SelectionType::Lines => {
                // Lines selection: entire rows are selected.
                row >= ns.row && row <= ne.row
            }
            SelectionType::Block => {
                // Apply side adjustment for sub-cell precision.
                let (start_row, start_col, end_row, end_col) = Self::apply_side_adjustment(ns, ne);
                aterm_types::selection::selection_contains_block(
                    row,
                    usize::from(col),
                    start_row,
                    usize::from(start_col),
                    end_row,
                    usize::from(end_col),
                )
            }
            // Simple, Semantic, and future variants use linear containment
            // with side adjustment.
            _ => {
                // Apply side adjustment for sub-cell precision.
                let (start_row, start_col, end_row, end_col) = Self::apply_side_adjustment(ns, ne);
                // After side adjustment, check for empty range.
                if start_row > end_row {
                    return false;
                }
                if start_row == end_row && start_col > end_col {
                    return false;
                }
                aterm_types::selection::selection_contains_linear(
                    row,
                    usize::from(col),
                    start_row,
                    usize::from(start_col),
                    end_row,
                    usize::from(end_col),
                )
            }
        }
    }

    /// Project the selection into side-adjusted grid coordinates.
    ///
    /// Returns `None` if no selection is active or if the selection is empty
    /// after side adjustment. `last_col` is the rightmost column index for
    /// `Lines` selection expansion.
    ///
    /// This performs the same side-aware normalization as the bridge's
    /// `Selection::ordered_bounds()` + `to_range()`, but expressed in the
    /// shared `(i32, u16)` coordinate space rather than bridge `Point`.
    pub fn project_range(&self, last_col: u16) -> Option<SelectionProjection> {
        if self.state == SelectionState::None {
            return None;
        }

        // Normalize anchors (reading order, block-aware).
        let ns = self.normalized_start();
        let ne = self.normalized_end();

        // Side adjustment: a Right-sided start means the selection begins at
        // the *next* cell; a Left-sided end means it stops at the *previous*.
        // (end_col = u16::MAX is clamped to last_col for Lines selections.)
        let (start_row, start_col, end_row, end_col) = Self::apply_side_adjustment(ns, ne);

        // After side adjustment the range may be empty (except Lines, which
        // always spans full rows regardless of sub-cell side position).
        if self.selection_type != SelectionType::Lines {
            if start_row > end_row {
                return None;
            }
            if start_row == end_row && start_col > end_col {
                return None;
            }
        }
        if start_row > end_row {
            return None;
        }

        match self.selection_type {
            SelectionType::Lines => Some(SelectionProjection {
                start_row,
                start_col: 0,
                end_row,
                end_col: last_col,
                is_block: false,
            }),
            SelectionType::Block => Some(SelectionProjection {
                start_row,
                start_col,
                end_row,
                end_col,
                is_block: true,
            }),
            // Simple, Semantic, and future variants are all linear.
            _ => Some(SelectionProjection {
                start_row,
                start_col,
                end_row,
                end_col,
                is_block: false,
            }),
        }
    }

    /// Expand the selection to include the entire cell on both ends.
    ///
    /// Sets the start side to `Left` and the end side to `Right`, ensuring
    /// that `project_range` will not clip partial cells.
    pub fn include_all(&mut self) {
        if self.state != SelectionState::None {
            self.start.side = SelectionSide::Left;
            self.end.side = SelectionSide::Right;
        }
    }

    /// Adjust raw anchor positions without changing sides or state.
    ///
    /// Used by scroll-rotation adapters that need to shift both anchors
    /// by a delta while preserving the selection type and side metadata.
    pub fn relocate(&mut self, start_row: i32, start_col: u16, end_row: i32, end_col: u16) {
        self.start.row = start_row;
        self.start.col = start_col;
        self.end.row = end_row;
        self.end.col = end_col;
    }

    /// Expand selection to semantic boundaries (for word selection).
    ///
    /// This is called after starting a Semantic selection to expand to word boundaries.
    pub fn expand_semantic(&mut self, start_col: u16, end_col: u16) {
        if self.state == SelectionState::InProgress
            && self.selection_type == SelectionType::Semantic
        {
            self.start.col = start_col;
            self.start.side = SelectionSide::Left;
            self.end.col = end_col;
            self.end.side = SelectionSide::Right;
        }
    }

    /// Expand selection to full lines.
    ///
    /// This is called for Lines selection type to select entire lines.
    pub fn expand_lines(&mut self, max_col: u16) {
        if self.state == SelectionState::InProgress && self.selection_type == SelectionType::Lines {
            self.start.col = 0;
            self.start.side = SelectionSide::Left;
            self.end.col = max_col;
            self.end.side = SelectionSide::Right;
        }
    }
}
