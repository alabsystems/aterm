// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn both_halves_of_a_wide_glyph_name_the_same_pair_of_columns() {
    assert_eq!(glyph_cell_span(7, false, false), (7, 7));
    assert_eq!(glyph_cell_span(4, true, false), (4, 5));
    assert_eq!(glyph_cell_span(5, false, true), (4, 5));
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
