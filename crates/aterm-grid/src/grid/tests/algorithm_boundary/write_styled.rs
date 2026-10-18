// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for write_ascii_run_styled, the fast path for styled ASCII output.

use super::*;

/// The styled ASCII run leaves the grid exactly as the same bytes written one
/// at a time through `write_char_wrap_styled` do — every cell's glyph,
/// colours and flags, every row's wrap flag, the cursor and the deferred wrap
/// — and reports the bytes written and the last one (an empty run keeps the
/// caller's).
#[test]
fn write_ascii_run_styled_matches_per_char_writes() {
    let styles = [
        (
            PackedColor::indexed(33),
            PackedColor::indexed(235),
            CellFlags::BOLD,
        ),
        (
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::ITALIC | CellFlags::UNDERLINE,
        ),
    ];
    #[expect(clippy::type_complexity, reason = "a test's case table")]
    let cases: &[(u16, u16, (u16, u16), &[u8])] = &[
        (3, 10, (0, 0), b""),
        (3, 10, (0, 0), b"Q"),
        (5, 10, (0, 0), b"Hello!"),
        (3, 5, (0, 0), b"ABCDE"),
        (3, 4, (0, 0), b"ABCDEFGH"),
        (3, 10, (0, 7), b"XYZ"),
        (2, 3, (0, 0), b"ABCDEF"),
    ];
    for (fg, bg, flags) in styles {
        for &(rows, cols, (row, col), text) in cases {
            let mut run = Grid::new(rows, cols);
            let mut per_char = Grid::new(rows, cols);
            for grid in [&mut run, &mut per_char] {
                grid.set_cursor(row, col);
            }
            let at = format!(
                "{:?} at ({row}, {col}) on {rows}x{cols}",
                String::from_utf8_lossy(text)
            );
            let mut last_byte = Some(b'~');
            let written = run.write_ascii_run_styled(text, fg, bg, flags, &mut last_byte);
            assert_eq!(written, text.len(), "{at}");
            assert_eq!(last_byte, Some(*text.last().unwrap_or(&b'~')), "{at}");
            for &byte in text {
                per_char.write_char_wrap_styled(char::from(byte), fg, bg, flags);
            }
            for r in 0..rows {
                assert_eq!(
                    run.row(r).map(|row| row.is_wrapped()),
                    per_char.row(r).map(|row| row.is_wrapped()),
                    "{at}: row {r} wrap"
                );
                for c in 0..cols {
                    let (a, b) = (run.cell(r, c).unwrap(), per_char.cell(r, c).unwrap());
                    assert_eq!(
                        (a.char(), a.colors(), a.flags()),
                        (b.char(), b.colors(), b.flags()),
                        "{at}: ({r}, {c})"
                    );
                }
            }
            assert_eq!(run.cursor(), per_char.cursor(), "{at}");
            assert_eq!(run.pending_wrap(), per_char.pending_wrap(), "{at}");
        }
    }
}
