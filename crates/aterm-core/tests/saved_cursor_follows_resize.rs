// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The DECSC / 1049 saved cursor follows its text through a resize, on the synchronous and the
//! offloaded path, for both slots.

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
    term.process(b"\x1b[6;3H\x1b7\x1b[10;1H");
    term.resize(8, 20);
    term.process(b"\x1b8X");
    assert_eq!(screen(&term)[3], "r5X", "{:?}", screen(&term));
}

/// A column a program saved past its output (`abc`, then a move to column 31) names no cell
/// of the line, only a distance from its start. A width change keeps that distance where the
/// new width has room and clamps it to the last column otherwise, as xterm, which never
/// moves the slot, restores it; pulling it back to the content's end (the live cursor's rule)
/// would put the next glyph right after `abc`.
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
/// cursor, which the shrink trimmed) is below the content it followed. A grow
/// hands the trimmed row back before it reveals history (`aterm_grid`'s rows
/// hand-back) and moves the slot down with the content it reveals over, so the
/// slot is back on its own row, the blank under the output. With the trim
/// forgotten (the law before the hand-back, which a replay of an older
/// recording runs) the grow reveals history only and the slot rides it, still
/// past the grid. Grown once more, the grid gains the blank row the slot
/// names, and DECRC lands there, not on the last line of output, which a slot
/// clamped into the grid at the reveal would overwrite.
#[test]
fn a_slot_past_the_grid_rides_a_reveal_below_the_content() {
    for path in [Path::Sync, Path::Offloaded] {
        for forget in [false, true] {
            let at = format!("{path:?} forget={forget}");
            let mut term = Terminal::new(6, 20);
            for i in 0..9 {
                term.process(format!("line {i}\r\n").as_bytes());
            }
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
                "{at}: the shrink trimmed its row"
            );
            if forget {
                term.grid_mut().forget_handback();
            }
            path.resize(&mut term, 9, 20);
            if forget {
                assert_eq!(
                    screen(&term)[0],
                    "line 0",
                    "{at}: the grow revealed history"
                );
                assert_eq!(slot(&term), Some((9, 0)), "{at}: still past the grid");
            } else {
                assert_eq!(
                    screen(&term)[0],
                    "line 1",
                    "{at}: the blank came back first"
                );
                assert_eq!(screen(&term)[7], "line 8", "{at}");
                assert_eq!(slot(&term), Some((8, 0)), "{at}: back on its own row");
            }
            path.resize(&mut term, 10, 20);
            term.process(b"\x1b8X");
            assert_eq!(screen(&term)[8], "line 8", "{at}");
            assert_eq!(screen(&term)[9], "X", "{at}");
        }
    }
}

/// A narrowing that rewraps more rows than fit below a cursor too near the top
/// pushes them off the bottom into history, as a rows-only shrink's bottom push
/// does, and the grow back puts them back under the screen (
/// it dropped them). A slot saved on one of them stays past the grid while its
/// line is in history, not clamped onto the last row kept, and comes back
/// with it: DECRC after the grow lands on its own cell.
#[test]
fn a_slot_on_a_line_the_narrowing_pushed_off_comes_back_with_it() {
    for path in [Path::Sync, Path::Offloaded] {
        let mut term = Terminal::new(4, 10);
        term.process(b"\x1b[2;1Haaaaaaaaaa\x1b[3;1Hbbbbbbbbbb\x1b[4;1Hcccccccccc");
        term.process(b"\x1b[4;3H\x1b7\x1b[H$ ");
        path.resize(&mut term, 4, 5);
        assert_eq!(
            screen(&term),
            ["$ ", "aaaaa", "aaaaa", "bbbbb"],
            "{path:?}: PRECONDITION: the rows past the cursor's went to history"
        );
        path.resize(&mut term, 7, 5);
        term.process(b"\x1b8X");
        assert_eq!(
            screen(&term),
            ["$ ", "aaaaa", "aaaaa", "bbbbb", "bbbbb", "ccXcc", "ccccc"],
            "{path:?}"
        );
    }
}

/// A slot past the grid (its blank row trimmed) stays past the grid through a
/// widening, which puts that blank row back under the output (the rows
/// hand-back lays the widened rows out as the screen held them): DECRC lands
/// on the blank row under the output, and once grown by a row, which reveals
/// history at the top, on the blank row again, not on the output's last line.
#[test]
fn a_slot_past_the_grid_stays_below_the_content_through_a_widening() {
    for path in [Path::Sync, Path::Offloaded] {
        let mut term = Terminal::new(6, 10);
        term.process(b"h0\r\nh1\r\n0123456789abcdefghij\r\nx2\r\nx3\r\n");
        term.process(b"\x1b7\x1b[A");
        path.resize(&mut term, 5, 10);
        path.resize(&mut term, 5, 20);
        assert_eq!(
            screen(&term),
            ["h1", "0123456789abcdefghij", "x2", "x3", ""],
            "{path:?}: PRECONDITION: the widening put the trimmed blank row back"
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

/// A move that follows the content inside an offloaded width change's window
/// keeps the history rows the re-attach owes (`FillDebt`): DECRC and the 1049
/// exit's restore (the slot moves with the rows), VT52 `ESC A` and the reverse
/// wrap (one row up from the cursor) land on the rows the synchronous resize
/// moved down, and the re-attach pulls the history over the blanks under them
/// as that resize did. A full screen widened in one step: every row ends where
/// the synchronous resize leaves it.
#[test]
fn a_move_that_follows_the_content_in_the_offload_window_keeps_the_owed_rows() {
    let fixture = |term: &mut Terminal| {
        for i in 0..12 {
            term.process(format!("line {i:02} {}\r\n", "x".repeat(24)).as_bytes());
        }
        term.process(b"\x1b[2J\x1b[H");
        term.process(format!("{}\r\n{}\r\n", "a".repeat(27), "b".repeat(27)).as_bytes());
        term.process(b"\x1b7top\r\n$ ");
    };
    for (name, before, step) in [
        ("DECRC", &b""[..], &b"\x1b8X"[..]),
        ("1049 exit after 47", b"\x1b[?47h", b"\x1b[?1049lX"),
        ("VT52 ESC A", b"", b"\x1b[?2l\x1bAX\x1b<"),
        ("reverse wrap", b"", b"\x1b[?45h\r\x08X"),
    ] {
        let mut sync = Terminal::with_scrollback(6, 20, 2, Scrollback::with_defaults());
        let mut offloaded = Terminal::with_scrollback(6, 20, 2, Scrollback::with_defaults());
        for term in [&mut sync, &mut offloaded] {
            fixture(term);
            term.process(before);
        }
        sync.resize(6, 40);
        let mut job = offloaded.resize_offloading_scrollback(6, 40);
        assert!(job.is_some(), "{name}: PRECONDITION: the widening detaches");
        assert!(
            !before.is_empty() || screen(&offloaded)[0] != screen(&sync)[0],
            "{name}: PRECONDITION: the window's screen owes the rows the synchronous one pulled"
        );
        sync.process(step);
        offloaded.process(step);
        while let Some(pending) = job {
            job = offloaded.finish_resize_offload(pending.reflow());
        }
        assert_eq!(screen(&offloaded), screen(&sync), "{name}: the screen");
        assert_eq!(
            offloaded.grid().cursor(),
            sync.grid().cursor(),
            "{name}: the cursor"
        );
    }
}
