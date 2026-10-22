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

/// Check if a cell at `(row, col)` is within a linear (non-block) selection.
///
/// The selection spans from `(start_row, start_col)` to `(end_row, end_col)` in
/// normalized (reading-order) coordinates. This is the shared containment predicate
/// used by both core and bridge selection implementations.
///
/// Column parameters use `usize` so both core (`u16` columns, promoted via `.into()`)
/// and bridge (`usize` columns) can call without narrowing casts.
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
///
/// Block selections include all cells whose row is between the min and max of
/// `start_row` and `end_row` AND whose column is between the min and max of
/// `start_col` and `end_col`.
///
/// Column parameters use `usize` so both core (`u16` columns, promoted via `.into()`)
/// and bridge (`usize` columns) can call without narrowing casts.
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
///
/// A double-width glyph — a CJK ideograph, an emoji, a ZWJ sequence — owns TWO
/// cells: a lead cell carrying the whole cluster's text and a blank continuation
/// cell carrying none. Nothing downstream of the selection can represent half of
/// one. The text extractors emit the entire cluster from the lead and nothing
/// from the continuation, so an edge that divides the pair paints one half a
/// column wide while the clipboard receives the whole character — what you see
/// stops being what you get, at both edges and in both drag directions.
///
/// So a glyph is INDIVISIBLE to a selection: it is selected iff either of its
/// cells is. Both sides of that promise widen their edges through here — the
/// highlight predicate (`TextSelection::contains_cell`) tests the pair, and the
/// copy's column walk (`visible_row_bounds_to_string`) starts at the lead and
/// stops at the continuation — so the painted run and the copied run are the
/// same run by construction rather than by two rules that happen to agree today.
///
/// `is_wide_lead` means the cell at `col` is the LEFT half; `is_wide_continuation`
/// means it is the RIGHT half. A cell can be neither (single width, `(col, col)`)
/// but never both; a continuation wins if a caller passes both, matching the
/// order the renderer resolves its own flags in.
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
mod tests {
    use super::*;

    #[test]
    fn both_halves_of_a_wide_glyph_name_the_same_pair_of_columns() {
        assert_eq!(glyph_cell_span(7, false, false), (7, 7));
        // Whichever half a selection edge lands on, the widened span is the same
        // — that identity is what makes the highlight and the copy agree.
        assert_eq!(glyph_cell_span(4, true, false), (4, 5));
        assert_eq!(glyph_cell_span(5, false, true), (4, 5));
        // A continuation at column 0 has no lead behind it, and a lead at the
        // last representable column has no continuation ahead: both stand alone
        // rather than wrapping to the opposite edge of the row.
        assert_eq!(glyph_cell_span(0, false, true), (0, 0));
        assert_eq!(glyph_cell_span(u16::MAX, true, false), (u16::MAX, u16::MAX));
    }

    /// Containment against the shapes it names, over every point and both
    /// anchors with row -2..=2 and col 0..=4 (15,625 point-anchor triples,
    /// each anchor order): a block is the normalized rectangle whichever
    /// anchor came first, and a linear span on one row is the inclusive column
    /// range — and, across rows, the reading-order span from the start cell
    /// through the end cell.
    #[test]
    fn containment_matches_the_normalized_shapes() {
        let points: Vec<(i32, usize)> = (-2..=2)
            .flat_map(|row| (0..=4).map(move |col| (row, col)))
            .collect();
        for &(row, col) in &points {
            for &(r1, c1) in &points {
                for &(r2, c2) in &points {
                    let rect = row >= r1.min(r2)
                        && row <= r1.max(r2)
                        && col >= c1.min(c2)
                        && col <= c1.max(c2);
                    assert_eq!(
                        selection_contains_block(row, col, r1, c1, r2, c2),
                        rect,
                        "block ({r1},{c1})..({r2},{c2}) at ({row},{col})"
                    );
                    assert_eq!(
                        selection_contains_block(row, col, r2, c2, r1, c1),
                        rect,
                        "block ({r2},{c2})..({r1},{c1}) at ({row},{col})"
                    );
                    if (r1, c1) <= (r2, c2) {
                        let span = (r1, c1) <= (row, col) && (row, col) <= (r2, c2);
                        assert_eq!(
                            selection_contains_linear(row, col, r1, c1, r2, c2),
                            span,
                            "linear ({r1},{c1})..({r2},{c2}) at ({row},{col})"
                        );
                        if r1 == r2 && row == r1 {
                            assert_eq!(span, c1 <= col && col <= c2);
                        }
                    }
                }
            }
        }
    }
}
