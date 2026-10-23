// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Mouse-based text selection state machine.

use std::cmp::Ordering;

pub use aterm_types::selection::{SelectionSide, SelectionType};

/// Side-aware projection of a selection into concrete grid coordinates.
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

#[cfg(test)]
mod tests;

/// Selection lifecycle state.
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
}

impl PartialOrd for SelectionAnchor {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SelectionAnchor {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        (self.row, self.col, self.side).cmp(&(other.row, other.col, other.side))
    }
}

/// Text selection state machine (`docs/DESIGN-selection-custody-2026-08-21.md` §5.2).
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
    /// Whether an endpoint was CLAMPED to the history floor because its content
    /// was evicted from scrollback — the selection still names real text, but
    /// less of it than the user picked.
    truncated: bool,
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
            truncated: false,
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
    #[cfg(test)]
    #[inline]
    pub(crate) const fn is_complete(&self) -> bool {
        matches!(self.state, SelectionState::Complete)
    }

    /// Check if selection is in progress (mouse button held).
    #[cfg(test)]
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
    #[inline]
    pub const fn start(&self) -> SelectionAnchor {
        self.start
    }

    /// Get the raw (un-normalized) end anchor.
    #[inline]
    pub const fn end(&self) -> SelectionAnchor {
        self.end
    }

    /// Whether an endpoint was clamped to the history floor after scrollback eviction.
    #[must_use]
    #[inline]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    /// Whether this selection overlaps the absolute row band `lo_abs..=hi_abs`.
    #[must_use]
    #[inline]
    pub fn intersects_absolute_band(&self, live_top_abs: u64, lo_abs: u64, hi_abs: u64) -> bool {
        if self.state == SelectionState::None {
            return false;
        }
        let base = i64::try_from(live_top_abs).unwrap_or(i64::MAX);
        let to_abs = |row: i32| base.saturating_add(i64::from(row));
        let (a, b) = (to_abs(self.start.row), to_abs(self.end.row));
        let (sel_lo, sel_hi) = if a <= b { (a, b) } else { (b, a) };
        let lo = i64::try_from(lo_abs).unwrap_or(i64::MAX);
        let hi = i64::try_from(hi_abs).unwrap_or(i64::MAX);
        sel_lo <= hi && lo <= sel_hi
    }

    /// Re-floor the selection after scrollback eviction without a scroll delta (e.g. a retention-limit shrink or memory-budget trim).
    pub fn truncate_to_floor(&mut self, oldest_abs: u64, live_top_abs: u64) -> bool {
        let oldest = i64::try_from(oldest_abs).unwrap_or(i64::MAX);
        let live_top = i64::try_from(live_top_abs).unwrap_or(i64::MAX);
        let min_row = i32::try_from(oldest.saturating_sub(live_top)).unwrap_or(i32::MIN);
        debug_assert!(
            min_row <= 0,
            "history floor cannot sit below the live top: min_row={min_row}"
        );
        self.clamp_head_to_min_row(min_row.min(0))
    }

    /// Clamp an endpoint evicted below `min_row` (the deepest retained row,
    /// including tiered history) onto `min_row`, or clear and return `false`
    /// when both endpoints are below `min_row`.
    fn clamp_head_to_min_row(&mut self, min_row: i32) -> bool {
        if self.state == SelectionState::None {
            return true;
        }
        let lo = self.start.row.min(self.end.row);
        let hi = self.start.row.max(self.end.row);
        if lo >= min_row {
            return true;
        }
        if min_row >= 0 || hi < min_row {
            self.clear();
            return false;
        }
        let anchor = if self.start.row <= self.end.row {
            &mut self.start
        } else {
            &mut self.end
        };
        anchor.row = min_row;
        if self.selection_type != SelectionType::Block {
            anchor.col = 0;
            anchor.side = SelectionSide::Left;
        }
        self.truncated = true;
        if self.side_adjusted_bounds().is_none() {
            self.clear();
            return false;
        }
        true
    }

    /// Get the normalized start (the anchor that comes first).
    #[inline]
    pub fn normalized_start(&self) -> SelectionAnchor {
        if self.selection_type == SelectionType::Block {
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
    #[inline]
    pub fn normalized_end(&self) -> SelectionAnchor {
        if self.selection_type == SelectionType::Block {
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
    #[inline]
    pub fn normalized_bounds(&self) -> (i32, u16, i32, u16) {
        let start = self.normalized_start();
        let end = self.normalized_end();
        (start.row, start.col, end.row, end.col)
    }

    /// Apply sub-cell side adjustment to a normalized anchor pair.
    #[inline]
    fn apply_side_adjustment(
        ns: SelectionAnchor,
        ne: SelectionAnchor,
    ) -> Option<(i32, u16, i32, u16)> {
        let start_col = if ns.side == SelectionSide::Right {
            ns.col.saturating_add(1)
        } else {
            ns.col
        };
        let (end_row, end_col) = if ne.side == SelectionSide::Left {
            if ne.col > 0 {
                (ne.row, ne.col - 1)
            } else {
                // Saturating here would instead select that entire row.
                (ne.row.checked_sub(1)?, u16::MAX)
            }
        } else {
            (ne.row, ne.col)
        };
        Some((ns.row, start_col, end_row, end_col))
    }

    /// Get side-adjusted selection bounds matching the visual highlight.
    #[inline]
    pub fn side_adjusted_bounds(&self) -> Option<(i32, u16, i32, u16)> {
        if self.state == SelectionState::None {
            return None;
        }

        let ns = self.normalized_start();
        let ne = self.normalized_end();

        if self.selection_type == SelectionType::Lines {
            return Some((ns.row, 0, ne.row, u16::MAX));
        }
        let (start_row, start_col, end_row, end_col) = Self::apply_side_adjustment(ns, ne)?;
        if start_row > end_row || (start_row == end_row && start_col > end_col) {
            return None;
        }

        Some((start_row, start_col, end_row, end_col))
    }

    /// Start a new selection.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "text_selection_lifecycle",
            action = "Start",
            project = "conformance_text_selection::project"
        )
    )]
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
        self.truncated = false;
    }

    /// Update the selection endpoint (during mouse drag).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "text_selection_lifecycle",
            action = "Update",
            project = "conformance_text_selection::project"
        )
    )]
    pub fn update_selection(&mut self, row: i32, col: u16, side: SelectionSide) {
        if self.state == SelectionState::InProgress {
            self.end = SelectionAnchor::new(row, col, side);
        }
    }

    /// Complete the selection (mouse button released).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "text_selection_lifecycle",
            action = "Complete",
            project = "conformance_text_selection::project"
        )
    )]
    pub fn complete_selection(&mut self) {
        if self.state == SelectionState::InProgress {
            self.state = SelectionState::Complete;
        }
    }

    /// Clear the selection.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "text_selection_lifecycle",
            action = "Clear",
            project = "conformance_text_selection::project"
        )
    )]
    pub fn clear(&mut self) {
        self.state = SelectionState::None;
        self.truncated = false;
    }

    /// Extend an existing complete selection.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "text_selection_lifecycle",
            action = "Extend",
            project = "conformance_text_selection::project"
        )
    )]
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

    /// Shift selection anchors by `-delta` rows on a terminal scroll.
    pub fn adjust_for_scroll(&mut self, delta: i32, max_rows: i32, floor: i32) -> bool {
        if self.state == SelectionState::None {
            return true;
        }

        if max_rows <= 0 {
            self.clear();
            return false;
        }

        let new_start_row = self.start.row.saturating_sub(delta);
        let new_end_row = self.end.row.saturating_sub(delta);

        let min_row = floor.saturating_neg();

        if new_start_row >= max_rows || new_end_row >= max_rows {
            self.clear();
            return false;
        }

        self.start.row = new_start_row;
        self.end.row = new_end_row;
        self.clamp_head_to_min_row(min_row)
    }

    /// Re-anchor endpoints across a cursor-anchored rows-only shrink.
    pub fn adjust_for_rows_shrink(
        &mut self,
        kept_rows: i32,
        removed_rows: i32,
        floor: i32,
    ) -> bool {
        if self.state == SelectionState::None {
            return true;
        }
        if kept_rows <= 0 || removed_rows <= 0 {
            return self.adjust_for_scroll(0, kept_rows, floor);
        }
        self.adjust_for_scroll(removed_rows, kept_rows, floor)
    }

    /// Translate both anchor rows into a presentation coordinate space.
    #[inline]
    pub const fn translate_rows_for_presentation(&mut self, delta: i32) {
        self.start.row = self.start.row.saturating_add(delta);
        self.end.row = self.end.row.saturating_add(delta);
    }

    /// Re-anchor selection endpoints across a row insertion of `inserted` rows before a fixed footer at `boundary_row`.
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
        if new_start_row >= max_rows || new_end_row >= max_rows {
            self.clear();
            return false;
        }

        self.start.row = new_start_row;
        self.end.row = new_end_row;
        self.clamp_head_to_min_row(min_row)
    }

    /// Whether `(row, col)` is selected, expanding wide glyphs via [`glyph_cell_span`](aterm_types::selection::glyph_cell_span).
    #[inline]
    pub fn contains_cell(
        &self,
        row: i32,
        col: u16,
        is_wide: bool,
        is_wide_continuation: bool,
    ) -> bool {
        let (lead, continuation) =
            aterm_types::selection::glyph_cell_span(col, is_wide, is_wide_continuation);
        self.contains(row, lead) || (continuation != lead && self.contains(row, continuation))
    }

    /// Check if a cell is within the selection.
    #[inline]
    pub fn contains(&self, row: i32, col: u16) -> bool {
        let Some((start_row, start_col, end_row, end_col)) = self.side_adjusted_bounds() else {
            return false;
        };

        match self.selection_type {
            SelectionType::Block => aterm_types::selection::selection_contains_block(
                row,
                usize::from(col),
                start_row,
                usize::from(start_col),
                end_row,
                usize::from(end_col),
            ),
            _ => aterm_types::selection::selection_contains_linear(
                row,
                usize::from(col),
                start_row,
                usize::from(start_col),
                end_row,
                usize::from(end_col),
            ),
        }
    }

    /// Project the selection into side-adjusted grid coordinates.
    #[inline]
    pub fn project_range(&self, last_col: u16) -> Option<SelectionProjection> {
        let (start_row, start_col, end_row, end_col) = self.side_adjusted_bounds()?;
        if self.selection_type == SelectionType::Lines {
            return Some(SelectionProjection {
                start_row,
                start_col: 0,
                end_row,
                end_col: last_col,
                is_block: false,
            });
        }
        Some(SelectionProjection {
            start_row,
            start_col,
            end_row,
            end_col,
            is_block: self.selection_type == SelectionType::Block,
        })
    }

    /// Expand the selection to include the entire cell on both ends.
    #[inline]
    pub fn include_all(&mut self) {
        if self.state != SelectionState::None {
            if self.start <= self.end {
                self.start.side = SelectionSide::Left;
                self.end.side = SelectionSide::Right;
            } else {
                self.start.side = SelectionSide::Right;
                self.end.side = SelectionSide::Left;
            }
        }
    }

    /// Adjust raw anchor positions without changing sides or state.
    #[inline]
    pub const fn relocate(&mut self, start_row: i32, start_col: u16, end_row: i32, end_col: u16) {
        self.start.row = start_row;
        self.start.col = start_col;
        self.end.row = end_row;
        self.end.col = end_col;
    }

    /// Expand selection to semantic boundaries (for word selection).
    #[inline]
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
    #[inline]
    pub fn expand_lines(&mut self, max_col: u16) {
        if self.state == SelectionState::InProgress && self.selection_type == SelectionType::Lines {
            self.start.col = 0;
            self.start.side = SelectionSide::Left;
            self.end.col = max_col;
            self.end.side = SelectionSide::Right;
        }
    }
}
