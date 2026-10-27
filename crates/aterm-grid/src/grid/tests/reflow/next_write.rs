// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A resize carries reflowed cursor cells and real pending wraps. Without reflow, an ordinary
//! cursor cell cut off by narrowing clamps without inventing a wrap onto the next row.

use super::super::super::reflow::ReflowMode;
use super::super::super::*;

/// Each row's text, `~` in front of a row that continues the one above.
fn rows(grid: &Grid) -> Vec<String> {
    (0..grid.rows())
        .map(|r| {
            let row = grid.row(r).unwrap();
            let mark = if row.is_wrapped() { "~" } else { "" };
            format!("{mark}{row}")
        })
        .collect()
}

struct Case {
    name: &'static str,
    size: (u16, u16),
    text: &'static str,
    /// Where the cursor is moved after the text, if anywhere.
    moved_to: Option<(u16, u16)>,
    /// Printed from there.
    overprint: &'static str,
    /// Margins `(left, right, row)`: the cursor then waits on that row's right-margin cell with a
    /// wrap pending, as a print up to the margin leaves it (the grid's plain print ignores margins).
    margin_wrap: Option<(u16, u16, u16)>,
    resized: (u16, u16),
    mode: ReflowMode,
    /// The cursor's row, column and pending wrap after the resize.
    cursor: (u16, u16, bool),
    /// The rows after an `X` is printed there.
    then: &'static [&'static str],
}

const fn case(name: &'static str, size: (u16, u16), text: &'static str) -> Case {
    Case {
        name,
        size,
        text,
        moved_to: None,
        overprint: "",
        margin_wrap: None,
        resized: size,
        mode: ReflowMode::Enabled,
        cursor: (0, 0, false),
        then: &[],
    }
}

const CASES: &[Case] = &[
    Case {
        resized: (3, 10),
        cursor: (0, 5, false),
        then: &["abcdeX", "", ""],
        ..case(
            "a wrap pending at the edge takes the next column on a widening",
            (3, 5),
            "abcde",
        )
    },
    Case {
        resized: (3, 10),
        mode: ReflowMode::Disabled,
        cursor: (0, 5, false),
        then: &["abcdeX", "", ""],
        ..case("and without reflow", (3, 5), "abcde")
    },
    Case {
        resized: (3, 10),
        cursor: (1, 0, false),
        then: &["abcdefghij", "~X", ""],
        ..case(
            "a cursor just past a wrapped line's content, widened onto a full row, starts a row of its own",
            (3, 6),
            "abcdefghij",
        )
    },
    Case {
        resized: (3, 5),
        cursor: (1, 0, false),
        then: &["abcde", "~X", ""],
        ..case(
            "a cursor just past the content, narrowed onto a full row, starts a row of its own",
            (3, 10),
            "abcde",
        )
    },
    Case {
        moved_to: Some((0, 5)),
        resized: (4, 5),
        cursor: (1, 0, false),
        then: &["abcde", "~X", "fgh", ""],
        ..case(
            "and with a line below, which the new row pushes down rather than continues",
            (4, 10),
            "abcde\nfgh",
        )
    },
    Case {
        moved_to: Some((1, 4)),
        resized: (4, 10),
        cursor: (1, 0, false),
        then: &["abcdefghij", "~X", "XYZ", ""],
        ..case(
            "a cursor just past a wrapped line, widened onto a full row above a line",
            (4, 6),
            "abcdefghij\nXYZ",
        )
    },
    Case {
        resized: (3, 4),
        cursor: (1, 3, true),
        then: &["abcd", "~efgh", "~X"],
        ..case(
            "a wrap pending at the edge stays pending where the rewrap ends the line",
            (3, 8),
            "abcdefgh",
        )
    },
    Case {
        moved_to: Some((0, 0)),
        overprint: "ABCDEFGHIJ",
        resized: (3, 5),
        cursor: (1, 4, true),
        then: &["ABCDE", "~FGHIJ", "~Xlm"],
        ..case(
            "an edge wrap over a line that goes on stays on the row of the glyph it follows",
            (3, 10),
            "abcdefghijklm",
        )
    },
    Case {
        moved_to: Some((0, 7)),
        resized: (3, 20),
        cursor: (0, 7, false),
        then: &["abc    X", "", ""],
        ..case(
            "a cursor in a gap past the content keeps its column",
            (3, 10),
            "abc",
        )
    },
    Case {
        moved_to: Some((0, 7)),
        resized: (3, 5),
        cursor: (1, 2, false),
        then: &["abc", "~  X", ""],
        ..case(
            "a cursor in a gap past the new width keeps its place in its line, on a row of its own",
            (3, 10),
            "abc",
        )
    },
    Case {
        moved_to: Some((0, 7)),
        resized: (4, 5),
        cursor: (1, 2, false),
        then: &["abc", "~  X", "def", ""],
        ..case("and with a line below", (4, 10), "abc\ndef")
    },
    Case {
        moved_to: Some((0, 7)),
        resized: (4, 5),
        cursor: (1, 2, false),
        then: &["", "~  X", "def", ""],
        ..case("and on an empty row", (4, 10), "\ndef")
    },
    Case {
        moved_to: Some((0, 7)),
        resized: (4, 5),
        mode: ReflowMode::Disabled,
        cursor: (0, 4, false),
        then: &["abc X", "def", "", ""],
        ..case(
            "without reflow, a cursor past the new width is clamped onto the last column, as xterm does",
            (4, 10),
            "abc\ndef",
        )
    },
    Case {
        moved_to: Some((3, 7)),
        resized: (4, 5),
        mode: ReflowMode::Disabled,
        cursor: (3, 4, false),
        then: &["r0", "r1", "r2", "abc X"],
        ..case(
            "and on the bottom row, which the next glyph does not scroll",
            (4, 10),
            "r0\nr1\nr2\nabc",
        )
    },
    Case {
        resized: (3, 4),
        mode: ReflowMode::Disabled,
        cursor: (0, 3, true),
        then: &["abcd", "~X", ""],
        ..case(
            "without reflow, a wrap pending at the edge stays pending on a narrowing",
            (3, 8),
            "abcdefgh",
        )
    },
    Case {
        resized: (5, 5),
        cursor: (0, 4, true),
        then: &["abcde", "~X", "", "", ""],
        ..case("a rows-only resize keeps the wrap", (3, 5), "abcde")
    },
    Case {
        cursor: (0, 4, true),
        then: &["abcde", "~X", ""],
        ..case("a same-size resize keeps the wrap", (3, 5), "abcde")
    },
    Case {
        moved_to: Some((0, 4)),
        resized: (3, 10),
        cursor: (0, 4, false),
        then: &["abcdX", "", ""],
        ..case("a cursor moved onto a glyph stays on it", (3, 5), "abcde")
    },
    Case {
        margin_wrap: Some((0, 2, 0)),
        resized: (3, 12),
        cursor: (0, 2, true),
        then: &["abc", "~X", ""],
        ..case(
            "a wrap pending at a right margin stays pending on its column",
            (3, 10),
            "abc",
        )
    },
];

/// Print `text` from the cursor, a `\n` starting the next row as CR LF does.
fn print(grid: &mut Grid, text: &str) {
    for ch in text.chars() {
        if ch == '\n' {
            grid.carriage_return();
            grid.line_feed();
        } else {
            grid.write_char_wrap(ch);
        }
    }
}

fn run(c: &Case) -> Grid {
    let mut grid = Grid::new(c.size.0, c.size.1);
    print(&mut grid, c.text);
    if let Some((row, col)) = c.moved_to {
        grid.set_cursor(row, col);
    }
    for ch in c.overprint.chars() {
        grid.write_char_wrap(ch);
    }
    if let Some((left, right, row)) = c.margin_wrap {
        grid.set_horizontal_margins(left, right);
        grid.set_cursor(row, right);
        grid.set_pending_wrap(true);
    }
    grid
}

#[test]
fn a_resize_puts_the_next_glyph_after_the_text_the_cursor_follows() {
    let mut wrong = Vec::new();
    for c in CASES {
        let mut grid = run(c);
        grid.resize_with_reflow_mode(c.resized.0, c.resized.1, c.mode);
        grid.assert_invariants();
        let cursor = (grid.cursor_row(), grid.cursor_col(), grid.pending_wrap());
        grid.write_char_wrap('X');
        let then = rows(&grid);
        if cursor != c.cursor || then != c.then {
            wrong.push(format!(
                "{}: cursor {cursor:?}, want {:?}; after an X {then:?}, want {:?}",
                c.name, c.cursor, c.then
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn a_width_round_trip_puts_the_next_write_back() {
    let mut wrong = Vec::new();
    // Each case: its name, the size, the text, where the cursor is moved, the width it goes
    // through, and whether the cursor and its wrap come back exactly. An edge wrap widened takes
    // the next column with no wrap, which a narrowing back carries into a row of its own: the
    // next glyph goes where it went before, from the start of that row.
    for (name, size, text, moved_to, via, exact) in [
        (
            "an edge wrap through a widening",
            (3, 5),
            "abcde",
            None,
            10,
            false,
        ),
        (
            "an edge wrap through a narrowing",
            (3, 8),
            "abcdefgh",
            None,
            4,
            true,
        ),
        (
            "past a wrapped line, through a full row",
            (3, 6),
            "abcdefghij",
            None,
            10,
            true,
        ),
        (
            "in a gap, through a widening",
            (3, 10),
            "abc",
            Some((0, 7)),
            20,
            true,
        ),
        (
            "in a gap, through a narrowing that puts it on a row of its own",
            (3, 10),
            "abc",
            Some((0, 7)),
            5,
            true,
        ),
        (
            "just past the content, through a narrowing onto a full row",
            (4, 10),
            "abcde\nfgh",
            Some((0, 5)),
            5,
            true,
        ),
    ] {
        let start = || {
            let mut grid = Grid::new(size.0, size.1);
            print(&mut grid, text);
            if let Some((row, col)) = moved_to {
                grid.set_cursor(row, col);
            }
            grid
        };
        let state = |grid: &Grid| (grid.cursor_row(), grid.cursor_col(), grid.pending_wrap());
        let mut grid = start();
        let before = state(&grid);
        grid.resize(size.0, via);
        grid.resize(size.0, size.1);
        grid.assert_invariants();
        let after = state(&grid);
        if exact && after != before {
            wrong.push(format!("{name}: cursor {after:?}, want {before:?}"));
        }
        grid.write_char_wrap('X');
        let mut unresized = start();
        unresized.write_char_wrap('X');
        let (got, want) = (rows(&grid), rows(&unresized));
        if got != want {
            wrong.push(format!("{name}: after an X {got:?}, want {want:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
