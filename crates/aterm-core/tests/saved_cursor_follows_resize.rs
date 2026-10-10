// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The DECSC / 1049 saved cursor follows its text through a resize.
//!
//! A full-screen app enters the alternate screen (1049 saves the main-screen
//! cursor), the window is resized, the app exits (1049 restores it) and the
//! shell prints its prompt. The resize rewraps or re-seats the saved primary
//! screen, so a slot left at its pre-resize row and column named other text:
//! the prompt was drawn over earlier output and the next keystrokes overwrote
//! it. Every reference terminal moves the slot with its text: xterm
//! (`AdjustSavedCursor`, cursor.c), Alacritty (`grow_lines`), kitty
//! (`screen_resize` tracks the saved cursors through `rewrap`), Ghostty
//! (`Screen.resize` reflows the saved cursor's pin) and WezTerm. aterm now does
//! too, on the synchronous and the offloaded path, for both slots; a bare DECRC
//! and the 1049 exit still clamp what they read to the grid.

use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::Terminal;

fn screen(term: &Terminal) -> Vec<String> {
    let grid = term.grid();
    (0..grid.rows())
        .map(|r| grid.row_text(r).expect("visible row"))
        .collect()
}

fn offloaded_resize(term: &mut Terminal, rows: u16, cols: u16) {
    let mut job = term.resize_offloading_scrollback(rows, cols);
    while let Some(pending) = job {
        job = term.finish_resize_offload(pending.reflow());
    }
}

/// How a test resizes: the synchronous `Terminal::resize`, or the GUI's
/// offloaded width change (a rows-only resize is the plain one there).
#[derive(Clone, Copy, Debug)]
enum Path {
    Sync,
    Offloaded,
}

impl Path {
    fn resize(self, term: &mut Terminal, rows: u16, cols: u16) {
        match self {
            Self::Sync => term.resize(rows, cols),
            Self::Offloaded => offloaded_resize(term, rows, cols),
        }
    }
}

/// A terminal as the window builds one (ring and tiered store), or a plain one.
fn terminal(tiered: bool, rows: u16, cols: u16) -> Terminal {
    if tiered {
        Terminal::with_scrollback(rows, cols, 1000, Scrollback::with_defaults())
    } else {
        Terminal::new(rows, cols)
    }
}

/// Narrowed while vim was up: the rewrap moves the command line down a row,
/// and the prompt lands on the row after it.
#[test]
fn a_narrowing_under_the_alternate_screen_moves_the_saved_cursor_with_its_line() {
    for (tiered, path) in [
        (false, Path::Sync),
        (true, Path::Sync),
        (true, Path::Offloaded),
    ] {
        let mut term = terminal(tiered, 8, 40);
        term.process(b"$ ls\r\n");
        term.process(b"Cargo.toml  README.md  src  target  tests\r\n");
        term.process(b"$ vim notes.txt\r\n");
        term.process(b"\x1b[?1049h\x1b[2J\x1b[Hvim screen");
        path.resize(&mut term, 8, 20);
        term.process(b"\x1b[?1049l$ ");
        assert_eq!(
            screen(&term)[..6],
            [
                "$ ls",
                "Cargo.toml  README.m",
                "d  src  target  test",
                "s",
                "$ vim notes.txt",
                "$ ",
            ],
            "tiered={tiered} {path:?}"
        );
        assert_eq!(
            (term.cursor().row, term.cursor().col),
            (5, 2),
            "tiered={tiered} {path:?}"
        );
    }
}

/// Grown taller while vim was up: the grow reveals history above the saved
/// screen, and the prompt follows its line down instead of overwriting `out3`.
#[test]
fn a_grow_under_the_alternate_screen_moves_the_saved_cursor_with_the_reveal() {
    for tiered in [false, true] {
        let mut term = terminal(tiered, 4, 20);
        for i in 0..6 {
            term.process(format!("out{i}\r\n").as_bytes());
        }
        term.process(b"$ vim\r\n\x1b[?1049h\x1b[2J\x1b[Hvim");
        term.resize(8, 20);
        term.process(b"\x1b[?1049l$ ");
        assert_eq!(
            screen(&term),
            [
                "out0", "out1", "out2", "out3", "out4", "out5", "$ vim", "$ "
            ],
            "tiered={tiered}"
        );
    }
}

/// The GUI's flow: a tall window with deep history, vim, a maximize, vim exits.
#[test]
fn a_maximize_under_the_alternate_screen_puts_the_prompt_under_its_command() {
    let mut term = terminal(true, 24, 80);
    for i in 0..100 {
        term.process(format!("output line {i}\r\n").as_bytes());
    }
    term.process(b"$ vim notes.txt\r\n\x1b[?1049h\x1b[2J\x1b[Hvim");
    offloaded_resize(&mut term, 50, 80);
    term.process(b"\x1b[?1049l$ ");
    let rows = screen(&term);
    let at = usize::from(term.cursor().row);
    assert_eq!(rows[at], "$ ", "{rows:?}");
    assert_eq!(rows[at - 1], "$ vim notes.txt", "{rows:?}");
    assert_eq!(rows[at - 2], "output line 99", "{rows:?}");
}

/// No alternate screen: DECSC, a narrowing, DECRC. The slot follows the line
/// it was saved on, so `X` is appended to `tail` instead of overwriting a
/// rewrapped row.
#[test]
fn a_bare_decsc_follows_its_line_through_a_rewrap() {
    for (tiered, path) in [
        (false, Path::Sync),
        (true, Path::Sync),
        (true, Path::Offloaded),
    ] {
        let mut term = terminal(tiered, 6, 20);
        term.process(b"0123456789abcdefghijKLMNOPQRST\r\ntail\x1b7");
        // Move away so DECRC has to restore something.
        term.process(b"\x1b[1;1H");
        path.resize(&mut term, 6, 10);
        term.process(b"\x1b8X");
        assert_eq!(
            screen(&term)[..4],
            ["0123456789", "abcdefghij", "KLMNOPQRST", "tailX"],
            "tiered={tiered} {path:?}"
        );
    }
}

/// The narrowing outgrows the screen and pushes its top row into history: the
/// slot moves up with the rows that stay.
#[test]
fn a_bare_decsc_follows_its_line_when_the_rewrap_pushes_rows_into_history() {
    for (tiered, path) in [
        (false, Path::Sync),
        (true, Path::Sync),
        (true, Path::Offloaded),
    ] {
        let mut term = terminal(tiered, 5, 20);
        term.process(b"0123456789abcdefghijKLMNOPQRST\r\ntail\x1b7\r\nmore\r\nmore2");
        path.resize(&mut term, 5, 10);
        term.process(b"\x1b8X");
        assert_eq!(
            screen(&term),
            ["abcdefghij", "KLMNOPQRST", "tailX", "more", "more2"],
            "tiered={tiered} {path:?}"
        );
    }
}

/// The round trip is exact: narrowed and widened back while vim was up, the
/// prompt lands where it would have without the resizes.
#[test]
fn a_width_round_trip_under_the_alternate_screen_leaves_the_saved_cursor_in_place() {
    for (tiered, path) in [
        (false, Path::Sync),
        (true, Path::Sync),
        (true, Path::Offloaded),
    ] {
        // Four rows: the narrowing pushes `$ ls` into history and the widening
        // pulls it back (on the offloaded path at the re-attach).
        let mut term = terminal(tiered, 4, 40);
        term.process(b"$ ls\r\nCargo.toml  README.md  src  target  tests\r\n$ vim");
        term.process(b"\x1b[?1049h\x1b[2J\x1b[Hvim");
        path.resize(&mut term, 4, 17);
        path.resize(&mut term, 4, 40);
        term.process(b"\x1b[?1049l!");
        assert_eq!(
            screen(&term)[..4],
            [
                "$ ls",
                "Cargo.toml  README.md  src  target  test",
                "s",
                "$ vim!",
            ],
            "tiered={tiered} {path:?}"
        );
    }
}

/// The alternate screen's own slot follows the alternate screen's rows: a
/// shrink that demotes its top rows moves the saved cursor up with its row.
#[test]
fn the_alternate_screens_saved_cursor_follows_a_demoting_shrink() {
    let mut term = Terminal::new(10, 20);
    term.process(b"\x1b[?1049h");
    for r in 0..10u16 {
        term.process(format!("\x1b[{};1Hr{r}", r + 1).as_bytes());
    }
    // Save on `r5`, then park the cursor on the last row so the shrink demotes.
    term.process(b"\x1b[6;3H\x1b7\x1b[10;1H");
    term.resize(8, 20);
    term.process(b"\x1b8X");
    assert_eq!(screen(&term)[3], "r5X", "{:?}", screen(&term));
}

/// A column a program saved past its output (`abc`, then a move to column 31)
/// names no cell of the line, only a distance from its start. A width change
/// keeps that distance where the new width has room (a widening always does)
/// and clamps it to the last column otherwise, as xterm, which never moves the
/// slot, restores it. Pulled back to the content's end (the live cursor's
/// rule), the next glyph landed right after `abc`.
#[test]
fn a_saved_column_past_the_lines_content_keeps_its_distance() {
    for (cols, expected) in [(100u16, 30usize), (60, 30), (20, 19)] {
        for alternate in [false, true] {
            for (tiered, path) in [
                (false, Path::Sync),
                (true, Path::Sync),
                (true, Path::Offloaded),
            ] {
                let mut term = terminal(tiered, 6, 80);
                term.process(b"$ ls\r\nabc\x1b[2;31H");
                let (save, restore): (&[u8], &[u8]) = if alternate {
                    (b"\x1b[?1049h\x1b[Hvim", b"\x1b[?1049lX")
                } else {
                    (b"\x1b7\x1b[4;1Hmore", b"\x1b8X")
                };
                term.process(save);
                path.resize(&mut term, 6, cols);
                term.process(restore);
                let at = format!("{cols} columns, alternate={alternate} tiered={tiered} {path:?}");
                assert_eq!(screen(&term)[0], "$ ls", "{at}");
                assert_eq!(
                    screen(&term)[1],
                    format!("abc{}X", " ".repeat(expected - 3)),
                    "{at}"
                );
            }
        }
    }
}

/// A slot a shrink left past the grid (DECSC on the blank row below the
/// cursor, which the shrink trimmed) is below the content it followed, and a
/// grow that reveals history moves it down with that content, still past the
/// grid. Grown once more, with no history left to reveal, the grid gains the
/// blank row the slot names, and DECRC lands there, not on the last line of
/// output, which a slot clamped into the grid at the reveal would overwrite.
#[test]
fn a_slot_past_the_grid_rides_a_reveal_below_the_content() {
    for path in [Path::Sync, Path::Offloaded] {
        let mut term = Terminal::new(6, 20);
        for i in 0..9 {
            term.process(format!("line {i}\r\n").as_bytes());
        }
        // DECSC on the blank last row, then up onto `line 8`.
        term.process(b"\x1b7\x1b[A");
        path.resize(&mut term, 5, 20);
        let slot = |term: &Terminal| {
            term.checkpoint_carry(0)
                .and_then(|cp| cp.saved_cursor_main)
                .map(|saved| (saved.cursor_row, saved.cursor_col))
        };
        assert_eq!(
            slot(&term),
            Some((5, 0)),
            "{path:?}: the shrink trimmed its row"
        );
        path.resize(&mut term, 9, 20);
        assert_eq!(
            screen(&term)[0],
            "line 0",
            "{path:?}: the grow revealed history"
        );
        assert_eq!(slot(&term), Some((9, 0)), "{path:?}: still past the grid");
        path.resize(&mut term, 10, 20);
        term.process(b"\x1b8X");
        assert_eq!(screen(&term)[8], "line 8", "{path:?}");
        assert_eq!(screen(&term)[9], "X", "{path:?}");
    }
}

/// A narrowing that rewraps more rows than fit below the cursor drops them
/// (the Native rule keeps the cursor's rows). A slot saved on a dropped line
/// stays past the grid where that line went, not clamped onto the last row
/// kept, so a grow followed by DECRC lands on a blank row below the kept
/// output instead of overwriting it.
#[test]
fn a_slot_on_a_line_the_narrowing_dropped_stays_past_the_grid() {
    for path in [Path::Sync, Path::Offloaded] {
        let mut term = Terminal::new(4, 10);
        term.process(b"\x1b[2;1Haaaaaaaaaa\x1b[3;1Hbbbbbbbbbb\x1b[4;1Hcccccccccc");
        term.process(b"\x1b[4;3H\x1b7\x1b[H$ ");
        path.resize(&mut term, 4, 5);
        assert_eq!(
            screen(&term),
            ["$ ", "aaaaa", "aaaaa", "bbbbb"],
            "{path:?}: PRECONDITION: the rows past the cursor's were dropped"
        );
        path.resize(&mut term, 7, 5);
        term.process(b"\x1b8X");
        assert_eq!(
            screen(&term),
            ["$ ", "aaaaa", "aaaaa", "bbbbb", "", "  X", ""],
            "{path:?}"
        );
    }
}

/// A slot past the grid (its blank row trimmed) rides the deficit fill that
/// bottom-anchors a widening, still past the grid: grown by a row once the
/// history is spent, DECRC lands on the blank row under the output, not on
/// its last line.
#[test]
fn a_slot_past_the_grid_rides_the_deficit_fill_below_the_content() {
    for path in [Path::Sync, Path::Offloaded] {
        let mut term = Terminal::new(6, 10);
        term.process(b"h0\r\nh1\r\n0123456789abcdefghij\r\nx2\r\nx3\r\n");
        // DECSC on the blank last row, then up onto `x3`.
        term.process(b"\x1b7\x1b[A");
        path.resize(&mut term, 5, 10);
        path.resize(&mut term, 5, 20);
        assert_eq!(
            screen(&term),
            ["h0", "h1", "0123456789abcdefghij", "x2", "x3"],
            "{path:?}: PRECONDITION: the widening pulled the history back"
        );
        path.resize(&mut term, 6, 20);
        term.process(b"\x1b8X");
        assert_eq!(
            screen(&term),
            ["h0", "h1", "0123456789abcdefghij", "x2", "x3", "X"],
            "{path:?}"
        );
    }
}
