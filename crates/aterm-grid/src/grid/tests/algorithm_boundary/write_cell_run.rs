// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for write_cell_run — the bulk fill path for repeated characters.

use super::super::super::*;

/// The bulk fill leaves the grid exactly as the same styled byte written one
/// cell at a time does: every cell (glyph, colours, flags), the cursor, the
/// deferred wrap and the scroll at the screen bottom, for runs that stop short
/// of, exactly at and past the line end, wrap several rows, or are empty.
#[test]
fn write_cell_run_matches_per_char_writes() {
    let fg = PackedColor::indexed(1);
    let colors = Cell::convert_colors(fg, PackedColor::DEFAULT_BG);
    for start_row in [0u16, 2] {
        for count in [0usize, 1, 5, 9, 10, 11, 12, 20, 26] {
            let mut bulk = Grid::new(3, 10);
            let mut per_char = Grid::new(3, 10);
            for grid in [&mut bulk, &mut per_char] {
                grid.set_cursor(start_row, 0);
            }
            let mut last_byte = None;
            let written = bulk.write_cell_run(b'#', count, colors, CellFlags::BOLD, &mut last_byte);
            for _ in 0..count {
                per_char.write_char_wrap_styled('#', fg, PackedColor::DEFAULT_BG, CellFlags::BOLD);
            }
            let at = format!("{count} from row {start_row}");
            assert_eq!(written, count, "{at}");
            assert_eq!(last_byte, (count > 0).then_some(b'#'), "{at}");
            for row in 0..3 {
                for col in 0..10 {
                    let (b, c) = (
                        bulk.cell(row, col).unwrap(),
                        per_char.cell(row, col).unwrap(),
                    );
                    assert_eq!(
                        (b.char(), b.colors(), b.flags()),
                        (c.char(), c.colors(), c.flags()),
                        "{at}: ({row}, {col})"
                    );
                }
            }
            assert_eq!(bulk.cursor(), per_char.cursor(), "{at}");
            assert_eq!(bulk.pending_wrap(), per_char.pending_wrap(), "{at}");
        }
    }
}
