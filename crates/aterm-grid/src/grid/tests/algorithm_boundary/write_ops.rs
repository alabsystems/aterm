// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::extra::CellCoord;

/// The ASCII blast leaves the grid exactly as the same bytes written one at a
/// time through `write_char_wrap` do: every cell, every row's wrap flag, the
/// cursor and the deferred wrap — from the origin or mid-row, stopping short
/// of, exactly at or past the line end, wrapping, and scrolling at the bottom.
#[test]
fn write_ascii_blast_matches_per_char_writes() {
    #[expect(clippy::type_complexity, reason = "a test's case table")]
    let cases: &[(u16, u16, (u16, u16), &[u8])] = &[
        (3, 10, (0, 0), b""),
        (3, 10, (0, 0), b"X"),
        (5, 10, (0, 0), b"Hello!"),
        (3, 5, (0, 0), b"ABCDE"),
        (3, 4, (0, 0), b"ABCDEFGH"),
        (2, 5, (0, 0), b"ABCDEFGHIJKLMNO"),
        (3, 10, (0, 5), b"Hello"),
        (2, 3, (0, 0), b"ABCDEF"),
    ];
    for &(rows, cols, (row, col), text) in cases {
        let mut blast = Grid::new(rows, cols);
        let mut per_char = Grid::new(rows, cols);
        for grid in [&mut blast, &mut per_char] {
            grid.set_cursor(row, col);
        }
        let at = format!(
            "{:?} at ({row}, {col}) on {rows}x{cols}",
            String::from_utf8_lossy(text)
        );
        assert_eq!(blast.write_ascii_blast(text), text.len(), "{at}");
        for &byte in text {
            per_char.write_char_wrap(char::from(byte));
        }
        for r in 0..rows {
            let line = |g: &Grid| g.row(r).map(|row| (row.to_string(), row.is_wrapped()));
            assert_eq!(line(&blast), line(&per_char), "{at}: row {r}");
        }
        assert_eq!(blast.cursor(), per_char.cursor(), "{at}");
        assert_eq!(blast.pending_wrap(), per_char.pending_wrap(), "{at}");
        blast.resolve_pending_wrap();
        per_char.resolve_pending_wrap();
        assert_eq!(
            screen_rows(&blast),
            screen_rows(&per_char),
            "{at}: after the wrap resolves"
        );
    }
}

fn screen_rows(grid: &Grid) -> Vec<String> {
    (0..grid.rows())
        .map(|r| grid.row(r).map(ToString::to_string).unwrap_or_default())
        .collect()
}

/// ECH clears the extras of the cells it blanks, ICH shifts extras right with
/// their cells (the inserted blanks carry none), DCH drops the deleted cells'
/// extras and shifts the rest left. The fixtures attach extras through
/// `cell_extra_mut`, which sets the cell's HAS_EXTRAS claim as every engine
/// path does: an entry under a cell without it is an orphan these operations
/// drop (`Grid::drop_unclaimed_extras`).
#[test]
fn char_ops_carry_extras_with_their_cells() {
    type Op = fn(&mut Grid, u16);
    let cases: &[(&str, Op, &[u16], &[u16])] = &[
        ("ECH 2 at col 2", |g, n| g.erase_chars(n), &[1, 2, 3], &[1]),
        ("ICH 2 at col 2", |g, n| g.insert_chars(n), &[1, 4], &[1, 6]),
        (
            "DCH 2 at col 2",
            |g, n| g.delete_chars(n),
            &[1, 3, 6],
            &[1, 4],
        ),
    ];
    for &(name, op, before, after) in cases {
        let mut grid = Grid::new(3, 10);
        for c in "ABCDEFGH".chars() {
            grid.write_char(c);
        }
        for &col in before {
            grid.cell_extra_mut(0, col).add_combining('\u{0301}');
        }
        grid.set_cursor(0, 2);
        op(&mut grid, 2);
        let got: Vec<u16> = (0..10)
            .filter(|&col| grid.extras().get(CellCoord::new(0, col)).is_some())
            .collect();
        assert_eq!(got, after, "{name}");
    }
}
