// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! No operation leaves half a wide glyph, and the blanks it leaves are the right ones: the
//! regressions the differential VT oracle found once astream-term's fold modelled wide glyphs
//! (aterm-conformance `vt_oracle`, B13 step 4). Each answer is read off xterm's and Ghostty's
//! source, named at the test; each test was run against the tree before its fix and failed.

use aterm_core::grid::{CellFlags, Grid};
use aterm_core::terminal::Terminal;

fn term(cols: u16, rows: u16, bytes: &str) -> Terminal {
    let mut t = Terminal::new(rows, cols);
    t.process(bytes.as_bytes());
    t
}

/// Which part of a glyph each cell of row `row` holds: `L` a lead, `S` a spacer, `.` neither.
fn halves(grid: &Grid, row: u16) -> String {
    (0..grid.cols())
        .map(|c| {
            if grid.is_wide_continuation_at(row, c) {
                'S'
            } else if grid
                .cell(row, c)
                .is_some_and(|cell| cell.flags().contains(CellFlags::WIDE))
            {
                'L'
            } else {
                '.'
            }
        })
        .collect()
}

/// The first column of row `row` holding half a wide glyph without its other half.
fn torn(grid: &Grid, row: u16) -> Option<u16> {
    let h: Vec<char> = halves(grid, row).chars().collect();
    (0..h.len()).find_map(|c| {
        let whole = match h[c] {
            'L' => h.get(c + 1) == Some(&'S'),
            'S' => c > 0 && h[c - 1] == 'L',
            _ => true,
        };
        (!whole).then(|| u16::try_from(c).expect("a column fits u16"))
    })
}

fn text(grid: &Grid, row: u16) -> String {
    let line: String = (0..grid.cols())
        .filter(|&c| !grid.is_wide_continuation_at(row, c))
        .map(|c| match grid.resolved_char(row, c).unwrap_or(' ') {
            '\0' => ' ',
            ch => ch,
        })
        .collect();
    line.trim_end().to_string()
}

/// The palette background of `(row, col)`, `None` for the default one.
fn bg_index(grid: &Grid, row: u16, col: u16) -> Option<u8> {
    let colors = grid.cell(row, col).expect("a cell on the screen").colors();
    colors.bg_is_indexed().then(|| colors.bg_index())
}

#[test]
fn a_write_over_half_a_wide_glyph_blanks_the_other_half_in_the_writers_background() {
    // xterm's WriteText clears its `DamagedCurCells` with ClearInLine, whose ClearCells writes
    // the current colour pair; Ghostty's printCell clears the other half to `blankCell`, the
    // pen's background. aterm blanked it to the default cell on every print lane while its own
    // erases already took the bce blank. Lanes: a styled ASCII byte, an ASCII run, a BMP wide
    // glyph, a run of wide glyphs, a glyph with style extras (the slow path).
    let base = "\u{4e2d}\u{6587}ab\x1b[44m";
    for (write, blanked) in [
        ("\x1b[1;2Hx", [0].as_slice()),
        ("\x1b[1;1Hx", &[1]),
        ("\x1b[1;2Hxy", &[0, 3]),
        ("\x1b[1;2H\u{3042}", &[0, 3]),
        ("\x1b[1;2H\u{3042}\u{3044}", &[0]),
        ("\x1b[4:3m\x1b[1;2Hx", &[0]),
    ] {
        let t = term(8, 1, &format!("{base}{write}"));
        let grid = t.grid();
        for &col in blanked {
            assert_eq!(
                (grid.resolved_char(0, col), bg_index(grid, 0, col)),
                (Some(' '), Some(4)),
                "{write:?}: the orphaned half at column {col} takes the writer's background"
            );
        }
        assert_eq!(
            torn(grid, 0),
            None,
            "{write:?}: half a glyph left: {}",
            halves(grid, 0)
        );
    }
    // With the default pen the bce blank is the default cell, as before.
    let t = term(8, 1, "\u{4e2d}\u{6587}\x1b[1;2Hx");
    assert_eq!(bg_index(t.grid(), 0, 0), None);
    // An RGB background reaches the orphan too.
    let t = term(8, 1, "\u{4e2d}\u{6587}\x1b[48;2;1;2;3m\x1b[1;2Hx");
    assert_eq!(t.grid().bg_rgb_at(0, 0), Some([1, 2, 3]));
    assert!(t.grid().cell(0, 0).is_some_and(|c| c.colors().bg_is_rgb()));
}

#[test]
fn a_styled_wide_glyph_with_one_column_left_before_the_right_margin_wraps() {
    // xterm's dotext measures what is left before the right margin (`width_available = right
    // + 1 - cur_col`) and wraps a glyph that does not fit; Ghostty's print does the same at
    // `right_limit - 1`. aterm's fast path did, but its slow path (style extras, IRM) took the
    // screen's edge for the line's end and wrote the glyph across the margin. The plain pen is
    // the control: it took the fast path and always wrapped.
    for pen in ["", "\x1b[4:5m", "\x1b[4h", "\x1b[48;2;1;2;3m"] {
        let t = term(
            26,
            2,
            &format!("\x1b[?69h\x1b[;14s{pen}\r\n{}\u{ac00}", "a".repeat(13)),
        );
        let grid = t.grid();
        assert_eq!(
            text(grid, 0),
            "a".repeat(13),
            "{pen:?}: the row scrolled up"
        );
        assert_eq!(text(grid, 1), "\u{ac00}", "{pen:?}: the glyph wrapped");
        assert_eq!(
            &halves(grid, 0)[13..15],
            "..",
            "{pen:?}: nothing straddles the margin"
        );
        assert_eq!(t.cursor().col, 2, "{pen:?}");
    }
}

#[test]
fn a_wide_glyph_on_a_screen_one_column_wide_prints_as_a_narrow_space() {
    // Ghostty's print: `printCell(0, .narrow)` in the pen, and the cursor moves as for a narrow
    // glyph (xterm's dotext gives up on the glyph). aterm blanked the cell, wrapped, and wrote
    // nothing, leaving the cursor at the start of the next row.
    let t = term(1, 3, "\x1b[4;31m\u{4e2d}");
    let grid = t.grid();
    let cell = grid.cell(0, 0).expect("the one cell");
    assert_eq!(text(grid, 0), "");
    assert!(
        cell.flags().contains(CellFlags::UNDERLINE),
        "the pen's rendition"
    );
    assert_eq!(halves(grid, 0), ".");
    assert_eq!(
        (t.cursor().row, t.cursor().col),
        (0, 0),
        "the cursor did not move on"
    );
    // The next glyph wraps from there, through the per-glyph and the run lanes alike.
    for glyphs in ["a\u{4e2d}b", "a\u{4e2d}\u{6587}b"] {
        let t = term(1, 4, glyphs);
        let rows: Vec<String> = (0..4).map(|r| text(t.grid(), r)).collect();
        let want = if glyphs.chars().count() == 3 {
            ["a", "", "b", ""]
        } else {
            ["a", "", "", "b"]
        };
        assert_eq!(rows, want, "{glyphs:?}");
    }
}

#[test]
fn a_scroll_between_margins_unpairs_the_glyphs_straddling_them_first() {
    // Ghostty's rowWillBeShifted turns a wide glyph straddling a margin into two blanks that
    // keep their renditions before the rows move. aterm repaired after the move alone, so a
    // lead left outside the left margin met the spacer of another row's glyph shifted in, and
    // the two read as one glyph.
    let t = term(6, 2, "a\u{30a2}\r\nb\u{ac00}\x1b[?69h\x1b[3;6s\x1b[S");
    let grid = t.grid();
    assert_eq!(
        (halves(grid, 0), halves(grid, 1)),
        ("......".into(), "......".into())
    );
    assert_eq!((text(grid, 0), text(grid, 1)), ("a".into(), "b".into()));
    // Each half keeps its own rendition: a red glyph straddling the right margin leaves two red
    // blanks, through SU, IL and DL alike.
    for op in ["\x1b[S", "\x1b[1;3H\x1b[L", "\x1b[1;3H\x1b[M"] {
        let t = term(
            7,
            2,
            &format!("\x1b[31mabc\u{4e2d}de\r\nxyzwvu\x1b[?69h\x1b[3;4s\x1b[0m{op}"),
        );
        let grid = t.grid();
        assert_eq!(halves(grid, 0), ".......", "{op:?}");
        let outside = grid.cell(0, 4).expect("the half outside the margins");
        assert!(
            outside.colors().fg_is_indexed(),
            "{op:?}: it keeps its colour"
        );
        assert_eq!(
            (outside.colors().fg_index(), text(grid, 0).chars().nth(4)),
            (1, Some(' '))
        );
    }
    // A glyph wholly inside the margins moves whole (the control).
    let t = term(6, 2, "\x1b[?69h\x1b[2;5s\x1b[2;2H\u{4e2d}\x1b[S");
    assert_eq!(
        (text(t.grid(), 0), halves(t.grid(), 0)),
        (" \u{4e2d}".into(), ".LS...".into())
    );
}

#[test]
fn dch_blanks_a_glyph_straddling_the_right_margin_whole() {
    // Ghostty's deleteChars splits the boundary at the right margin's edge
    // (`splitCellBoundary(right + 1)`), clearing both halves to the bce blank before the shift.
    // aterm blanked the spacer outside the margin and shifted the lead left, half a glyph.
    let t = term(6, 1, "xy\u{4e2d}z\x1b[?69h\x1b[1;3s\x1b[44m\x1b[P");
    let grid = t.grid();
    assert_eq!(
        (text(grid, 0), halves(grid, 0)),
        ("y   z".into(), "......".into())
    );
    for col in 1..=3 {
        assert_eq!(
            bg_index(grid, 0, col),
            Some(4),
            "column {col} is a bce blank"
        );
    }
    // ICH pushes that lead off the margin, so its spacer alone was blanked; it stays right.
    let t = term(6, 1, "xy\u{4e2d}z\x1b[?69h\x1b[1;3s\x1b[44m\x1b[@");
    assert_eq!(
        (text(t.grid(), 0), halves(t.grid(), 0)),
        (" xy z".into(), "......".into())
    );
}

#[test]
fn a_glyph_unpaired_at_a_margin_drops_its_marks() {
    // Ghostty's rowWillBeShifted clears a straddling lead's grapheme (`clearGrapheme`) with its
    // codepoint. aterm blanked the lead's character but kept its combining marks, which then rode
    // a blank: a stray accent or dakuten in the screen text and in copied text. Before the
    // scroll: an accented glyph straddling the left margin (its lead stays outside), a VS16
    // emoji the same, and an accented glyph straddling the right margin (its lead scrolls up).
    for (bytes, row, col, want) in [
        (
            "a\u{4e2d}\u{301}bcde\x1b[?69h\x1b[3;6s\x1b[S",
            0,
            1,
            "a     e",
        ),
        (
            "a\u{2764}\u{fe0f}bcde\x1b[?69h\x1b[3;6s\x1b[S",
            0,
            1,
            "a     e",
        ),
        (
            "\r\nabcd\u{4e2d}\u{301}x\x1b[?69h\x1b[1;5s\x1b[S",
            0,
            4,
            "abcd",
        ),
    ] {
        let t = term(8, 3, bytes);
        assert_eq!(t.row_text(0).as_deref(), Some(want), "{bytes:?}");
        assert_eq!(t.cell_grapheme(row, col).as_deref(), Some(" "), "{bytes:?}");
        let extra = t
            .grid()
            .cell_extra(0, u16::try_from(col).expect("a column fits u16"));
        assert!(
            extra.is_none_or(|e| e.combining().is_empty() && e.complex_char().is_none()),
            "{bytes:?}: the blank holds no marks"
        );
    }
    // A glyph wholly inside the margins keeps its accent through the scroll (the control).
    let t = term(8, 2, "\x1b[?69h\x1b[2;6s\x1b[2;3H\u{4e2d}\u{301}\x1b[S");
    assert_eq!(t.cell_grapheme(0, 2).as_deref(), Some("\u{4e2d}\u{301}"));
}
