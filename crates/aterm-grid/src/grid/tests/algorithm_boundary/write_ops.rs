// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::extra::CellCoord;

/// `write_ascii_blast` matches per-character `write_char_wrap` across row boundaries and scrolling.
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

/// ECH clears erased cells' extras, ICH shifts extras right, and DCH shifts remaining extras left.
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

/// A wrap onto a double-width row whose DECSLRM left margin lies past the row's `cols / 2`
/// columns lands on the row's last column, so each bulk writer finds room after it. At the
/// region bottom the margined scroll keeps the row double-width, and the writers used to wrap
/// there for ever: no room at column `left`, wrap, back to column `left` (input review
/// 2026-10-30).
#[test]
fn bulk_writers_finish_after_a_wrap_past_a_double_width_row() {
    type Writer = fn(&mut Grid, &[u8]) -> usize;
    const COLORS: PackedColors = PackedColors::DEFAULT;
    let writers: [(&str, Writer); 4] = [
        ("write_ascii_blast", |g, b| g.write_ascii_blast(b)),
        ("write_cell_run", |g, b| {
            g.write_cell_run(b[0], b.len(), COLORS, CellFlags::empty(), &mut None)
        }),
        ("write_ascii_run_styled_packed", |g, b| {
            g.write_ascii_run_styled_packed(b, COLORS, CellFlags::BOLD, &mut None)
        }),
        ("write_ascii_run_with_extras", |g, b| {
            let rgb = Some([1, 2, 3]);
            g.write_ascii_run_with_extras(
                b,
                COLORS,
                CellFlags::empty(),
                rgb,
                None,
                None,
                0,
                None,
                &mut None,
            )
        }),
    ];
    for (name, write) in writers {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // The saved crasher's state: the bottom row double-width (40 columns of 80), the
            // left margin at 45, and a wrap pending from column 39, left of the margin.
            let mut grid = Grid::new(24, 80);
            grid.set_horizontal_margins(45, 79);
            grid.set_cursor(23, 39);
            grid.set_row_line_size(23, crate::LineSize::DoubleWidth);
            grid.set_pending_wrap(true);
            let written = write(&mut grid, b"zzzz");
            let _ = tx.send((written, grid.cursor(), grid.cell(23, 39).map(Cell::char)));
        });
        let (written, cursor, last) = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("{name} returned (a minute without a return is a hang)"));
        assert_eq!(written, 4, "{name} writes the whole run");
        assert_eq!(
            (cursor.row, cursor.col),
            (23, 39),
            "{name}: on the row's last column"
        );
        assert_eq!(
            last,
            Some('z'),
            "{name}: the run lands on the row's last column"
        );
    }
}
