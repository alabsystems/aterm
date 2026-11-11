// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shared selection types for terminal text selection.

use std::cmp::Ordering;

/// Type of selection being performed.
#[non_exhaustive]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, Default)]
pub enum SelectionType {
    /// Character-by-character selection (single click + drag).
    #[default]
    Simple,
    /// Rectangular block selection (Alt + click + drag).
    Block,
    /// Semantic selection — words, URLs, etc.
    Semantic,
    /// Full line selection (triple-click).
    Lines,
}

/// Which side of a cell a selection anchor is on.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
#[non_exhaustive]
pub enum SelectionSide {
    /// Left side of the cell (before the character).
    #[default]
    Left,
    /// Right side of the cell (after the character).
    Right,
}

impl PartialOrd for SelectionSide {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SelectionSide {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (SelectionSide::Left, SelectionSide::Right) => Ordering::Less,
            (SelectionSide::Right, SelectionSide::Left) => Ordering::Greater,
            _ => Ordering::Equal,
        }
    }
}

/// Check if a cell at `(row, col)` is within a normalized linear (non-block) selection.
///
/// Callers must pass normalized reading-order bounds `(start_row, start_col) <= (end_row, end_col)`.
#[inline]
#[must_use]
pub const fn selection_contains_linear(
    row: i32,
    col: usize,
    start_row: i32,
    start_col: usize,
    end_row: i32,
    end_col: usize,
) -> bool {
    if row < start_row || row > end_row {
        return false;
    }
    if row == start_row && row == end_row {
        col >= start_col && col <= end_col
    } else if row == start_row {
        col >= start_col
    } else if row == end_row {
        col <= end_col
    } else {
        true
    }
}

/// Check if a cell at `(row, col)` is within a block (rectangular) selection.
#[inline]
#[must_use]
pub const fn selection_contains_block(
    row: i32,
    col: usize,
    start_row: i32,
    start_col: usize,
    end_row: i32,
    end_col: usize,
) -> bool {
    let (min_row, max_row) = if start_row <= end_row {
        (start_row, end_row)
    } else {
        (end_row, start_row)
    };
    let (min_col, max_col) = if start_col <= end_col {
        (start_col, end_col)
    } else {
        (end_col, start_col)
    };
    row >= min_row && row <= max_row && col >= min_col && col <= max_col
}

/// The inclusive column span of the ONE glyph that occupies `col`.
#[inline]
#[must_use]
pub const fn glyph_cell_span(
    col: u16,
    is_wide_lead: bool,
    is_wide_continuation: bool,
) -> (u16, u16) {
    if is_wide_continuation {
        // Saturating, not `col - 1`: a continuation at column 0 has no lead to
        // back up to (a wide glyph cannot start left of the row), so it stands
        // alone rather than wrapping to u16::MAX and selecting the far edge.
        (col.saturating_sub(1), col)
    } else if is_wide_lead {
        (col, col.saturating_add(1))
    } else {
        (col, col)
    }
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
