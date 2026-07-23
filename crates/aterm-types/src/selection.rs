// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shared selection types for terminal text selection.
//!
//! These types are used by both `aterm-core` (TLA+-verified text selection state machine)
//! and `aterm-alacritty-bridge` (Alacritty-compatible selection tracking). Extracting them
//! here eliminates duplicate definitions and manual conversion functions.

use std::cmp::Ordering;

/// Type of selection being performed.
///
/// All four types are standard across terminal implementations:
/// - Simple: character-by-character drag selection
/// - Block: rectangular/column selection (Alt+drag)
/// - Semantic: word/URL boundary selection (double-click)
/// - Lines: full-line selection (triple-click)
#[non_exhaustive]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, Default)]
pub enum SelectionType {
    /// Character-by-character selection (single click + drag).
    #[default]
    Simple,
    /// Rectangular block selection (Alt + click + drag).
    Block,
    /// Semantic selection — words, URLs, etc. (double-click).
    Semantic,
    /// Full line selection (triple-click).
    Lines,
}

/// Which side of a cell a selection anchor is on.
///
/// This determines whether a half-selected cell at a boundary is included.
/// Left means before the character; Right means after it.
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
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SelectionSide {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (SelectionSide::Left, SelectionSide::Right) => Ordering::Less,
            (SelectionSide::Right, SelectionSide::Left) => Ordering::Greater,
            _ => Ordering::Equal,
        }
    }
}

/// Check if a cell at `(row, col)` is within a linear (non-block) selection.
///
/// The selection spans from `(start_row, start_col)` to `(end_row, end_col)` in
/// normalized (reading-order) coordinates. This is the shared containment predicate
/// used by both core and bridge selection implementations.
///
/// Column parameters use `usize` so both core (`u16` columns, promoted via `.into()`)
/// and bridge (`usize` columns) can call without narrowing casts.
#[inline]
pub fn selection_contains_linear(
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
///
/// Block selections include all cells whose row is between the min and max of
/// `start_row` and `end_row` AND whose column is between the min and max of
/// `start_col` and `end_col`.
///
/// Column parameters use `usize` so both core (`u16` columns, promoted via `.into()`)
/// and bridge (`usize` columns) can call without narrowing casts.
#[inline]
pub fn selection_contains_block(
    row: i32,
    col: usize,
    start_row: i32,
    start_col: usize,
    end_row: i32,
    end_col: usize,
) -> bool {
    let min_row = start_row.min(end_row);
    let max_row = start_row.max(end_row);
    let min_col = start_col.min(end_col);
    let max_col = start_col.max(end_col);
    row >= min_row && row <= max_row && col >= min_col && col <= max_col
}

#[cfg(kani)]
mod kani_proofs {
    use super::{selection_contains_block, selection_contains_linear};

    #[kani::proof]
    fn block_selection_is_symmetric_across_anchor_order() {
        let row: i32 = kani::any();
        let col: usize = kani::any();
        let start_row: i32 = kani::any();
        let start_col: usize = kani::any();
        let end_row: i32 = kani::any();
        let end_col: usize = kani::any();

        kani::assume(col < 1024);
        kani::assume(start_col < 1024);
        kani::assume(end_col < 1024);

        let forward = selection_contains_block(row, col, start_row, start_col, end_row, end_col);
        let reverse = selection_contains_block(row, col, end_row, end_col, start_row, start_col);

        kani::assert(
            forward == reverse,
            "block containment must be independent of anchor ordering",
        );
    }

    #[kani::proof]
    fn linear_single_row_matches_inclusive_column_range() {
        let row: i32 = kani::any();
        let col: usize = kani::any();
        let start_col: usize = kani::any();
        let end_col: usize = kani::any();

        kani::assume(col < 1024);
        kani::assume(start_col < 1024);
        kani::assume(end_col < 1024);
        kani::assume(start_col <= end_col);

        let result = selection_contains_linear(row, col, row, start_col, row, end_col);
        let expected = col >= start_col && col <= end_col;

        kani::assert(
            result == expected,
            "single-row linear containment must match inclusive column bounds",
        );
    }

    #[kani::proof]
    fn block_selection_matches_normalized_rectangle() {
        let row: i32 = kani::any();
        let col: usize = kani::any();
        let start_row: i32 = kani::any();
        let start_col: usize = kani::any();
        let end_row: i32 = kani::any();
        let end_col: usize = kani::any();

        kani::assume(col < 1024);
        kani::assume(start_col < 1024);
        kani::assume(end_col < 1024);

        let min_row = start_row.min(end_row);
        let max_row = start_row.max(end_row);
        let min_col = start_col.min(end_col);
        let max_col = start_col.max(end_col);
        let result = selection_contains_block(row, col, start_row, start_col, end_row, end_col);
        let expected = row >= min_row && row <= max_row && col >= min_col && col <= max_col;

        kani::assert(
            result == expected,
            "block containment must match the normalized selection rectangle",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_type_default_is_simple() {
        assert_eq!(SelectionType::default(), SelectionType::Simple);
    }

    #[test]
    fn selection_side_default_is_left() {
        assert_eq!(SelectionSide::default(), SelectionSide::Left);
    }

    #[test]
    fn selection_side_ordering() {
        assert!(SelectionSide::Left < SelectionSide::Right);
        assert!(SelectionSide::Right > SelectionSide::Left);
        assert_eq!(
            SelectionSide::Left.cmp(&SelectionSide::Left),
            Ordering::Equal
        );
        assert_eq!(
            SelectionSide::Right.cmp(&SelectionSide::Right),
            Ordering::Equal
        );
    }

    #[test]
    fn linear_contains_single_line() {
        // Selection on line 5, cols 3..10
        assert!(selection_contains_linear(5, 5, 5, 3, 5, 10));
        assert!(selection_contains_linear(5, 3, 5, 3, 5, 10));
        assert!(selection_contains_linear(5, 10, 5, 3, 5, 10));
        assert!(!selection_contains_linear(5, 2, 5, 3, 5, 10));
        assert!(!selection_contains_linear(5, 11, 5, 3, 5, 10));
    }

    #[test]
    fn linear_contains_multi_line() {
        // Selection from (1, 5) to (3, 10)
        assert!(selection_contains_linear(2, 0, 1, 5, 3, 10)); // middle line, any col
        assert!(selection_contains_linear(1, 5, 1, 5, 3, 10)); // start boundary
        assert!(selection_contains_linear(1, 80, 1, 5, 3, 10)); // start line, after start col
        assert!(selection_contains_linear(3, 0, 1, 5, 3, 10)); // end line, before end col
        assert!(selection_contains_linear(3, 10, 1, 5, 3, 10)); // end boundary
        assert!(!selection_contains_linear(0, 5, 1, 5, 3, 10)); // before start line
        assert!(!selection_contains_linear(4, 0, 1, 5, 3, 10)); // after end line
        assert!(!selection_contains_linear(1, 4, 1, 5, 3, 10)); // start line, before start col
        assert!(!selection_contains_linear(3, 11, 1, 5, 3, 10)); // end line, after end col
    }

    #[test]
    fn block_contains() {
        // Block selection from (1, 5) to (3, 10)
        assert!(selection_contains_block(2, 7, 1, 5, 3, 10));
        assert!(selection_contains_block(1, 5, 1, 5, 3, 10));
        assert!(selection_contains_block(3, 10, 1, 5, 3, 10));
        assert!(!selection_contains_block(2, 4, 1, 5, 3, 10)); // wrong column
        assert!(!selection_contains_block(2, 11, 1, 5, 3, 10)); // wrong column
        assert!(!selection_contains_block(0, 7, 1, 5, 3, 10)); // wrong row
        assert!(!selection_contains_block(4, 7, 1, 5, 3, 10)); // wrong row
    }

    #[test]
    fn block_contains_reversed_columns() {
        // Block where start_col > end_col (user dragged left)
        assert!(selection_contains_block(2, 5, 1, 10, 3, 5));
        assert!(selection_contains_block(2, 10, 1, 10, 3, 5));
        assert!(!selection_contains_block(2, 4, 1, 10, 3, 5));
        assert!(!selection_contains_block(2, 11, 1, 10, 3, 5));
    }

    #[test]
    fn block_contains_reversed_rows_and_columns() {
        // Block where the user dragged up and left from the initial anchor.
        assert!(selection_contains_block(2, 7, 3, 10, 1, 5));
        assert!(selection_contains_block(1, 5, 3, 10, 1, 5));
        assert!(selection_contains_block(3, 10, 3, 10, 1, 5));
        assert!(!selection_contains_block(0, 7, 3, 10, 1, 5));
        assert!(!selection_contains_block(4, 7, 3, 10, 1, 5));
        assert!(!selection_contains_block(2, 4, 3, 10, 1, 5));
        assert!(!selection_contains_block(2, 11, 3, 10, 1, 5));
    }
}
