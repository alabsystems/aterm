// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::{CellSpan, LineCells};

#[test]
fn arithmetic_cell_lookup_matches_expansion_with_gaps_and_saturation() {
    let check = |cells: &LineCells| {
        let expanded: Vec<_> = cells.iter().collect();
        let bound = expanded.last().map_or(0, |cell| cell.end);
        let mut points = vec![0, 1, usize::MAX];
        for cell in &expanded {
            points.extend([
                cell.byte.saturating_sub(1),
                cell.byte,
                cell.byte.saturating_add(1),
                cell.end,
            ]);
        }
        points.sort_unstable();
        points.dedup();
        for len in [
            0,
            1,
            4,
            bound / 2,
            bound,
            bound.saturating_add(1),
            usize::MAX,
        ] {
            for &byte in &points {
                let mut eligible = expanded
                    .iter()
                    .rev()
                    .filter(|cell| cell.byte <= byte && cell.byte < len);
                let expected = (
                    eligible.next().copied(),
                    eligible.next().map_or(0, |cell| cell.end_col),
                );
                assert_eq!(
                    cells.cell_and_previous(byte, len),
                    expected,
                    "byte={byte} len={len} cells={expanded:?}"
                );
            }
        }
    };
    for runs in [0, 1, 2, 3, 7, 8, 9, 15, 16, 17, 65, 129] {
        let mut cells = LineCells::new();
        let mut byte = 3;
        for run in 0..runs {
            let stride = run % 3 + 1;
            let count = run % 7 + 1;
            let col = if run % 3 == 0 {
                usize::MAX - 3
            } else {
                run % 5
            };
            cells.push_repeated(byte..byte + stride, col, run % 4, count);
            byte += stride * count + run % 2;
        }
        check(&cells);
    }
    let mut saturated = LineCells::new();
    saturated.push_repeated(usize::MAX - 7..usize::MAX - 5, usize::MAX - 3, 2, 7);
    check(&saturated);

    // A full-width repetition count remains one inline run.
    let count = u32::MAX as usize;
    let cells = LineCells::identity(count);
    for byte in [0, 1, 7, count - 2, count, usize::MAX] {
        let at = byte.min(count - 1);
        let cell = |byte| CellSpan {
            byte,
            end: byte + 1,
            col: byte,
            end_col: byte + 1,
        };
        assert_eq!(
            cells.cell_and_previous(byte, usize::MAX),
            (Some(cell(at)), at)
        );
    }
}

fn spans(cells: &LineCells) -> Vec<(usize, usize, usize, usize)> {
    cells
        .iter()
        .map(
            |CellSpan {
                 byte,
                 end,
                 col,
                 end_col,
             }| (byte, end, col, end_col),
        )
        .collect()
}

/// A row of one shape is one run; a change of shape, a gap in the
/// columns or a gap in the bytes starts another, and every cell reads
/// back as pushed.
#[test]
fn cells_read_back_as_pushed_in_runs_of_one_shape() {
    let mut cells = LineCells::new();
    for i in 0..4 {
        cells.push(i * 3..i * 3 + 3, i * 2, 2);
    }
    assert_eq!(cells.runs().count(), 1, "four ideographs are one run");
    assert!(cells.tail.is_empty(), "held inline");
    cells.push(12..13, 8, 1);
    cells.push(13..14, 10, 1); // a column gap (an orphaned right half)
    cells.push(16..17, 11, 1); // a byte gap (text no cell holds)
    assert_eq!(cells.runs().count(), 4);
    assert_eq!(
        spans(&cells),
        vec![
            (0, 3, 0, 2),
            (3, 6, 2, 4),
            (6, 9, 4, 6),
            (9, 12, 6, 8),
            (12, 13, 8, 9),
            (13, 14, 10, 11),
            (16, 17, 11, 12),
        ]
    );
    assert_eq!(cells.end_col(), 12);
}

/// A cell with no text, or one that would overlap the previous cell's
/// text, is not recorded.
#[test]
fn empty_and_overlapping_cells_are_not_recorded() {
    let mut cells = LineCells::new();
    cells.push(0..3, 0, 2);
    cells.push(3..3, 2, 1);
    cells.push(2..4, 2, 1);
    assert_eq!(spans(&cells), vec![(0, 3, 0, 2)]);
}

/// Joining shifts a row's cells by the bytes and columns before it, and
/// a run that continues across the join stays one run.
#[test]
fn appended_cells_are_shifted_and_merged() {
    let mut row = LineCells::new();
    row.push(0..1, 0, 1);
    row.push(1..2, 1, 1);
    let mut joined = row.clone();
    joined.append_shifted(&row, 2, 2);
    assert_eq!(joined.runs().count(), 1, "four ASCII cells in a row");
    joined.append_shifted(&row, 4, 10);
    assert_eq!(
        spans(&joined),
        vec![
            (0, 1, 0, 1),
            (1, 2, 1, 2),
            (2, 3, 2, 3),
            (3, 4, 3, 4),
            (4, 5, 10, 11),
            (5, 6, 11, 12),
        ]
    );
}

/// A joined line stops short of its end when any row joined into it does, its
/// head or a continuation, wherever the joins pad or shift its cells.
#[test]
fn a_joined_line_stops_short_when_a_row_does() {
    let mut row = LineCells::new();
    row.push(0..1, 0, 1);
    let mut cut = row.clone();
    cut.mark_truncated();
    let mut whole = row.clone();
    whole.append_wrapped_row(&mut "x".to_string(), "y", &row, 4);
    assert!(!whole.is_truncated());
    for (head, continuation) in [(&cut, &row), (&row, &cut)] {
        let mut joined = head.clone();
        let mut line = "x".to_string();
        joined.append_wrapped_row(&mut line, "y", continuation, 4);
        assert!(joined.is_truncated(), "{head:?} + {continuation:?}");
        assert_eq!(line, "x   y", "the stride is unchanged");
    }
}

#[test]
fn batched_wrapped_padding_matches_scalar_cells_and_text() {
    for line_len in 0..10 {
        for row_start in 0..14 {
            for (byte, end, col, width) in [
                (0, 0, 0, 0),
                (0, 1, 0, 1),
                (0, 4, 0, 0),
                (4, 8, 2, 2),
                (8, 12, 0, 1),
                (0, 1, 9, 2),
                (usize::MAX - 1, usize::MAX, 0, 1),
                (0, 1, usize::MAX - 1, 2),
            ] {
                for cut in [false, true] {
                    let mut original = LineCells::new();
                    original.push(byte..end, col, width);
                    let mut next = LineCells::new();
                    next.push(0..2, 0, 1);
                    next.push(3..4, 4, 0);
                    if cut {
                        original.mark_truncated();
                        next.mark_truncated();
                    }
                    let mut scalar = original.clone();
                    let mut expected = "x".repeat(line_len);
                    for at in scalar.end_col()..row_start {
                        let byte = expected.len();
                        expected.push(' ');
                        scalar.push(byte..expected.len(), at, 1);
                    }
                    scalar.append_shifted(&next, expected.len(), row_start);
                    expected.push_str("é z");
                    let mut actual = "x".repeat(line_len);
                    original.append_wrapped_row(&mut actual, "é z", &next, row_start);
                    assert_eq!(actual, expected);
                    assert_eq!(
                        spans(&original),
                        spans(&scalar),
                        "line={line_len} row={row_start} head={byte}..{end} col={col} width={width}"
                    );
                    assert_eq!(original.is_truncated(), scalar.is_truncated());
                }
            }
        }
    }
}

/// A soft-wrapped row joins at its stride, whatever its text measures: after a
/// three-column row ending in a one-cell VS16 base the next row starts at column 3,
/// not 4, and a short or empty row is padded out with one-column spaces.
#[test]
fn a_wrapped_row_joins_at_its_stride() {
    let mut row0 = LineCells::new();
    row0.push(0..1, 0, 1);
    row0.push(1..2, 1, 1);
    row0.push(2..8, 2, 1); // U+2714 U+FE0F, one cell wide
    let mut row1 = LineCells::new();
    row1.push(0..1, 0, 1);
    let mut line = String::from("ab\u{2714}\u{fe0f}");
    let mut joined = row0;
    joined.append_wrapped_row(&mut line, "c", &row1, 3);
    joined.append_wrapped_row(&mut line, "", &LineCells::new(), 6);
    joined.append_wrapped_row(&mut line, "d", &row1, 9);
    assert_eq!(line, "ab\u{2714}\u{fe0f}c     d");
    assert_eq!(
        spans(&joined),
        vec![
            (0, 1, 0, 1),
            (1, 2, 1, 2),
            (2, 8, 2, 3),
            (8, 9, 3, 4),
            (9, 10, 4, 5),
            (10, 11, 5, 6),
            (11, 12, 6, 7),
            (12, 13, 7, 8),
            (13, 14, 8, 9),
            (14, 15, 9, 10),
        ]
    );
}

/// A column names the cell drawn over it (either column of a wide cell),
/// and a column no cell covers names none.
#[test]
fn a_column_names_the_cell_drawn_over_it() {
    let mut cells = LineCells::new();
    cells.push(0..4, 0, 2);
    cells.push(4..8, 2, 2);
    cells.push(8..9, 5, 1);
    let found: Vec<_> = (0..7).map(|col| cells.byte_at_column(col)).collect();
    assert_eq!(
        found,
        [Some(0), Some(0), Some(4), Some(4), None, Some(8), None]
    );
    let text = "\u{1f1e6}\u{1f1e7}x";
    assert_eq!(cells.columns_of(text, 0, 8), (0, 4));
    assert_eq!(cells.columns_of(text, 8, 9), (5, 6));
}

/// A column range takes the bytes of every cell drawn over any of its
/// columns, a wide cell either edge cuts whole, and names none where no
/// cell is drawn.
#[test]
fn a_column_range_takes_the_cells_drawn_over_it() {
    let mut cells = LineCells::new();
    cells.push(0..4, 0, 2);
    cells.push(4..8, 2, 2);
    cells.push(8..9, 5, 1);
    cells.push(9..10, 6, 1);
    cells.push_repeated(10..13, 7, 2, 3);
    for (cols, want) in [
        ((0, 1), Some(0..4)),
        ((1, 2), Some(0..4)),
        ((1, 3), Some(0..8)),
        ((3, 4), Some(4..8)),
        ((4, 5), None),
        ((4, 6), Some(8..9)),
        ((5, 7), Some(8..10)),
        ((8, 9), Some(10..13)),
        ((8, 10), Some(10..16)),
        ((6, 100), Some(9..19)),
        ((13, 14), None),
        ((0, 0), None),
        ((1, 1), None),
        ((3, 3), None),
        ((8, 8), None),
        ((2, 1), None),
    ] {
        assert_eq!(cells.bytes_in_columns(cols.0, cols.1), want, "{cols:?}");
    }
    assert_eq!(LineCells::new().bytes_in_columns(0, 10), None);
}
