// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Vi copy-mode keeps one frame wherever the viewport is scrolled and
//! whatever the program writes.
//!
//! A vi line is live-relative: line 0 is the top of the live screen and a
//! negative line is history counted back from it; the window draws the vi
//! cursor at `line + display_offset`. Three things broke that frame. The
//! grid's `BufferAccess` read every `line >= 0` through the viewport's row
//! lookup, so scrolled back three rows, `w` on live line 0 read the history
//! line three rows above and landed mid-word. `H`, `M` and `L` set live lines
//! 0, rows/2 and rows-1, so scrolled back they left the rows the window shows.
//! And output that scrolled the live screen left the vi cursor on its line
//! number while its text moved up. Driven here as the window drives it: real
//! VT output, a real scroll, and vi motions through `Terminal`.
//!
//! Output does not always move every line. A scroll region moves only its own
//! rows, so a vi cursor follows its text only when its row was in the region;
//! a fixed row below a region that scrolls into history (Codex's composer,
//! apt's progress bar) keeps its cursor. A resize relabels lines the way it
//! relabels the selection's, and a cursor whose line is gone (a shrink, a
//! cleared history) is clamped back onto the grid.

use aterm_core::terminal::{Terminal, TerminalBuilder};
use aterm_core::{ViBoundary, ViMotion, ViPoint};
use aterm_types::BufferAccess;

/// Seven lines on a four-row screen: three scroll into history (vi lines
/// -3..=-1) and four stay live (0..=3). Line `i`'s first word is `i + 1`
/// letters long, so a motion that reads any other line lands on another
/// column.
const LINES: [&str; 7] = [
    "a zz",
    "bb zz",
    "ccc zz",
    "dddd zz",
    "eeeee zz",
    "ffffff zz",
    "ggggggg zz",
];
const ROWS: u16 = 4;
const COLS: u16 = 12;
const HISTORY: i32 = 3;

/// The first word's length on vi line `line`.
fn first_word(line: i32) -> u16 {
    u16::try_from(line + HISTORY + 1).expect("a fixture line")
}

/// One motion through the window's entry point; the vi cursor after it.
fn press(term: &mut Terminal, motion: ViMotion) -> ViPoint {
    term.vi_motion(motion, ViBoundary::Grid);
    term.vi_cursor_point()
}

/// Put the vi cursor on (`line`, `col`) by motions alone: `0`, then `k`/`j`
/// from the line it is on, and `l`.
fn place(term: &mut Terminal, line: i32, col: u16) {
    let here = press(term, ViMotion::First).line;
    let step = if line < here {
        ViMotion::Up
    } else {
        ViMotion::Down
    };
    for _ in 0..line.abs_diff(here) {
        press(term, step);
    }
    for _ in 0..col {
        press(term, ViMotion::Right);
    }
    assert_eq!(
        term.vi_cursor_point(),
        ViPoint::new(line, col),
        "placing the cursor"
    );
}

/// `motion` from (`line`, `col`).
fn from(term: &mut Terminal, line: i32, col: u16, motion: ViMotion) -> ViPoint {
    place(term, line, col);
    press(term, motion)
}

#[test]
fn word_motions_read_the_vi_line_at_every_display_offset() {
    let mut term = Terminal::new(ROWS, COLS);
    term.process(LINES.join("\r\n").as_bytes());
    assert_eq!(
        i32::try_from(term.grid().scrollback_lines()).expect("small history"),
        HISTORY
    );
    term.vi_toggle();
    for offset in 0..=HISTORY {
        if offset > 0 {
            term.scroll_display(1);
        }
        assert_eq!(
            term.grid().display_offset(),
            usize::try_from(offset).expect("a fixture offset")
        );
        for line in -HISTORY..i32::from(ROWS) {
            let n = first_word(line);
            let at = |col| ViPoint::new(line, col);
            let ctx = format!("line {line} at display offset {offset}");
            // `w`: from the first word to the second.
            assert_eq!(
                from(&mut term, line, 0, ViMotion::SemanticRight),
                at(n + 1),
                "w on {ctx}"
            );
            // `e`: the first word's last letter; a one-letter word is already
            // its own end, so `e` goes on to the end of the second.
            let end = if n >= 2 { n - 1 } else { n + 2 };
            assert_eq!(
                from(&mut term, line, 0, ViMotion::SemanticRightEnd),
                at(end),
                "e on {ctx}"
            );
            // `b`: from the second word back to the first.
            assert_eq!(
                from(&mut term, line, n + 1, ViMotion::SemanticLeft),
                at(0),
                "b on {ctx}"
            );
            // `$`: the last column, on the same line.
            assert_eq!(
                from(&mut term, line, 0, ViMotion::Last),
                at(COLS - 1),
                "$ on {ctx}"
            );
            // Across lines, history into live and back: `w` from the last
            // word goes to the next line's first, `b` from a line's start to
            // the previous line's last word.
            if line + 1 < i32::from(ROWS) {
                assert_eq!(
                    from(&mut term, line, n + 1, ViMotion::SemanticRight),
                    ViPoint::new(line + 1, 0),
                    "w off the end of {ctx}"
                );
            }
            if line > -HISTORY {
                assert_eq!(
                    from(&mut term, line, 0, ViMotion::SemanticLeft),
                    ViPoint::new(line - 1, first_word(line - 1) + 1),
                    "b off the start of {ctx}"
                );
            }
        }
    }
}

/// `H`, `M` and `L` land on the top, middle and bottom rows the window shows:
/// scrolled back, the viewport's top row is vi line `-display_offset`. Once
/// the offset reaches the screen height, live line 0 is not on screen at all.
#[test]
fn high_middle_low_land_on_the_rows_the_window_shows() {
    let mut term = Terminal::new(ROWS, COLS);
    term.process(LINES.join("\r\n").as_bytes());
    term.vi_toggle();
    for offset in 0..=HISTORY {
        if offset > 0 {
            term.scroll_display(1);
        }
        for (motion, row) in [
            (ViMotion::High, 0),
            (ViMotion::Middle, i32::from(ROWS) / 2),
            (ViMotion::Low, i32::from(ROWS) - 1),
        ] {
            place(&mut term, 0, 2);
            assert_eq!(
                press(&mut term, motion),
                ViPoint::new(row - offset, 2),
                "{motion:?} at display offset {offset}"
            );
        }
    }
}

/// Output that scrolls the live screen carries the vi cursor with its text.
/// Each line that enters history moves the text one vi line up, so the cursor
/// moves with it; scrolled back, the viewport is re-pinned by the same count
/// and the window keeps drawing the cursor on the same row.
#[test]
fn output_that_scrolls_the_screen_carries_the_vi_cursor_with_its_text() {
    for offset in 0..HISTORY {
        let ctx = format!("display offset {offset}");
        let mut term = Terminal::new(ROWS, COLS);
        term.process(LINES.join("\r\n").as_bytes());
        term.vi_toggle();
        term.scroll_display(offset);
        // On the last letter of "ffffff" (live line 2).
        place(&mut term, 2, 5);
        term.process(b"\r\nhhhhhhhh zz");
        assert_eq!(
            term.vi_cursor_point(),
            ViPoint::new(1, 5),
            "one line, {ctx}"
        );
        // `b` reads the word under it: "ffffff" starts at column 0.
        assert_eq!(
            press(&mut term, ViMotion::SemanticLeft),
            ViPoint::new(1, 0),
            "b on \"ffffff\", {ctx}"
        );
        if offset > 0 {
            let drawn = 1 + i32::try_from(term.grid().display_offset()).expect("small offset");
            assert_eq!(drawn, 2 + offset, "the drawn row, {ctx}");
        }
        // Three more lines carry it into history.
        term.process(b"\r\ni\r\nj\r\nk");
        assert_eq!(
            term.vi_cursor_point(),
            ViPoint::new(-2, 0),
            "four lines, {ctx}"
        );
        assert_eq!(
            press(&mut term, ViMotion::SemanticRightEnd),
            ViPoint::new(-2, 5),
            "e on \"ffffff\" in history, {ctx}"
        );
    }
}

/// With history at its cap, a cursor whose text was evicted rests on the
/// oldest line history keeps.
#[test]
fn output_past_a_full_history_leaves_the_vi_cursor_on_the_oldest_line() {
    let mut term = TerminalBuilder::new()
        .size(ROWS, COLS)
        .ring_buffer_size(2)
        .build();
    term.process(LINES.join("\r\n").as_bytes());
    let cap = i32::try_from(term.grid().scrollback_lines()).expect("small history");
    assert_eq!(cap, 2, "history is at its cap");
    term.vi_toggle();
    place(&mut term, 0, 0);
    term.process(b"\r\n1\r\n2\r\n3\r\n4\r\n5");
    assert_eq!(
        term.grid().scrollback_lines(),
        2,
        "history stays at its cap"
    );
    assert_eq!(term.vi_cursor_point(), ViPoint::new(-cap, 0));
}

/// The text on vi line `line`, trailing blanks dropped.
fn text(term: &Terminal, line: i32) -> String {
    BufferAccess::line_text(term.grid(), line)
        .unwrap_or_default()
        .trim_end()
        .to_owned()
}

/// Codex's inline history on a six-row screen: two lines of history, four
/// transcript rows (`h1`..`h4`) and, below them, two fixed rows (a status
/// row and the composer the program's cursor sits in).
fn codex() -> Terminal {
    let mut term = Terminal::new(6, 20);
    term.process(b"o1\r\no2\r\nh1\r\nh2\r\nh3\r\nh4\r\nSTATUS\r\nCOMPOSER> hi");
    term
}

/// One Codex history insert: a region over the transcript rows, a line fed at
/// its bottom (its top row enters history), the region reset and the cursor
/// put back in the composer.
fn codex_insert(term: &mut Terminal, line: &str) {
    term.process(format!("\x1b[1;4r\x1b[4;1H\r\n{line}\x1b[r\x1b[6;13H").as_bytes());
}

/// A top-anchored region scrolled into history moves only its own rows (and
/// history); the rows below it stay where they are. A vi cursor on the
/// composer or the status row stays on it; one on a transcript row or in
/// history follows its text, at every display offset.
#[test]
fn a_region_scrolling_into_history_leaves_the_vi_cursor_on_a_fixed_row_below_it() {
    // Where the cursor starts, how many lines one insert moves it, its text.
    let probes = [
        (ViPoint::new(5, 12), 0, "COMPOSER> hi"),
        (ViPoint::new(4, 3), 0, "STATUS"),
        (ViPoint::new(2, 1), 1, "h3"),
        (ViPoint::new(-1, 1), 1, "o2"),
    ];
    for offset in 0..=1 {
        for (start, follows, on) in probes {
            let ctx = format!("the cursor on {on:?} at display offset {offset}");
            let mut term = codex();
            term.vi_toggle();
            assert_eq!(
                term.vi_cursor_point(),
                ViPoint::new(5, 12),
                "copy mode starts in the composer"
            );
            term.scroll_display(offset);
            place(&mut term, start.line, start.col);
            assert_eq!(text(&term, start.line), on, "{ctx}");
            for n in 1..=3 {
                codex_insert(&mut term, &format!("new{n}"));
                let at = ViPoint::new(start.line - follows * n, start.col);
                assert_eq!(term.vi_cursor_point(), at, "{n} inserts, {ctx}");
                assert_eq!(text(&term, at.line), on, "{n} inserts, {ctx}");
            }
            assert_eq!(
                term.grid().scrollback_lines(),
                5,
                "each insert put a row into history"
            );
            assert_eq!(
                text(&term, 3),
                "new3",
                "the newest line is the region's last row"
            );
        }
    }
}

/// apt's progress bar: a region over every row but the last, the bar on the
/// last row, output fed at the region's bottom. `L` puts the vi cursor on the
/// bar, and the output leaves it there.
#[test]
fn output_above_a_progress_bar_leaves_the_vi_cursor_on_the_bar() {
    let mut term = Terminal::new(5, 20);
    term.process(b"Get:1 foo\r\nGet:2 bar\r\nGet:3 baz\r\nFetched 3 kB");
    // DECSTBM homes the cursor, so apt saves and restores it around the
    // region and the bar.
    term.process(b"\x1b7\x1b[1;4r\x1b8\x1b7\x1b[5;1HProgress: [ 10%]\x1b8");
    term.vi_toggle();
    assert_eq!(press(&mut term, ViMotion::Low).line, 4);
    assert_eq!(press(&mut term, ViMotion::First), ViPoint::new(4, 0));
    assert_eq!(text(&term, 4), "Progress: [ 10%]");
    term.process(b"\r\nUnpacking foo\r\nSetting up foo");
    assert_eq!(
        term.grid().scrollback_lines(),
        2,
        "two rows entered history"
    );
    assert_eq!(term.vi_cursor_point(), ViPoint::new(4, 0));
    assert_eq!(text(&term, 4), "Progress: [ 10%]");
}

/// Two Codex frames in one read: an insert, then the viewport grown by a row
/// (`SU` in the old region, the rows below it redrawn), then an insert into
/// the smaller region. The second insert's history splice does not merge with
/// the first's, so the selection sees only an invalidation, but the band record
/// still says which rows moved: the composer and status rows stay, a
/// transcript row follows its text, and a line in history moves up by the
/// three lines that entered it.
#[test]
fn two_codex_frames_in_one_read_leave_the_vi_cursor_on_the_composer() {
    let frames = b"\x1b[1;4r\x1b[4;1H\r\nn1\x1b[r\
        \x1b[1;4r\x1b[1S\x1b[r\
        \x1b[4;1H\x1b[2KWORKING\x1b[5;1H\x1b[2KSTATUS\x1b[6;1H\x1b[2KCOMPOSER> hi\
        \x1b[1;3r\x1b[3;1H\r\nn2\x1b[r\x1b[6;13H";
    // Where the cursor starts, where it ends, the text it is on.
    let probes = [
        (ViPoint::new(5, 12), 5, "COMPOSER> hi"),
        (ViPoint::new(4, 3), 4, "STATUS"),
        (ViPoint::new(3, 1), 0, "h4"),
        (ViPoint::new(2, 1), -1, "h3"),
        (ViPoint::new(-1, 1), -4, "o2"),
    ];
    for (start, line, on) in probes {
        let mut term = codex();
        term.vi_toggle();
        place(&mut term, start.line, start.col);
        assert_eq!(text(&term, start.line), on);
        term.process(frames);
        assert_eq!(
            term.grid().scrollback_lines(),
            5,
            "three rows entered history"
        );
        assert_eq!(
            term.vi_cursor_point(),
            ViPoint::new(line, start.col),
            "the cursor on {on:?}"
        );
        assert_eq!(text(&term, line), on);
    }
}

/// One read can mix a region scrolling into history with other moves at the
/// top of the screen, which the band record composes or lists beside it. A
/// line in history moves up by exactly the lines that entered it; a row on the
/// screen rides each move in turn and, if it entered history, stays among the
/// lines that did.
#[test]
fn a_read_mixing_moves_at_the_top_keeps_the_vi_cursor_on_its_text() {
    // Where the cursor starts, where it ends, the text it ends on.
    type Probe = (i32, i32, &'static str);
    let cases: [(&[u8], &[Probe]); 4] = [
        // An insert, then DL at the transcript's top: `h1` entered history,
        // `h2` is gone, and the band record holds one move of two rows.
        (
            b"\x1b[1;4r\x1b[4;1H\r\nnew1\x1b[1;1H\x1b[M\x1b[r\x1b[6;13H",
            &[
                (-1, -2, "o2"),
                (0, -1, "h1"),
                (2, 0, "h3"),
                (5, 5, "COMPOSER> hi"),
            ],
        ),
        // An insert, then a reverse index at the screen's top: the screen
        // moves down a row and history stays.
        (
            b"\x1b[1;4r\x1b[4;1H\r\nnew1\x1b[r\x1b[1;1H\x1bM\x1b[6;13H",
            &[
                (-1, -2, "o2"),
                (0, -1, "h1"),
                (2, 2, "h3"),
                (4, 5, "STATUS"),
            ],
        ),
        // An insert, then a line fed at the bottom of a region below the
        // screen's top: only that region's rows move again.
        (
            b"\x1b[1;4r\x1b[4;1H\r\nnew1\x1b[2;4r\x1b[4;1H\r\nnew2\x1b[r\x1b[6;13H",
            &[(-1, -2, "o2"), (1, 0, "h2"), (3, 1, "h4"), (4, 4, "STATUS")],
        ),
        // DL, then a reverse index, at the screen's top, nothing into history:
        // `h1` is gone, so the cursor rests on the top row and rides the
        // reverse index down with `h2`.
        (
            b"\x1b[1;1H\x1b[M\x1bM\x1b[6;13H",
            &[(-1, -1, "o2"), (0, 1, "h2"), (5, 5, "COMPOSER> hi")],
        ),
    ];
    for (bytes, probes) in cases {
        for &(from_line, to_line, on) in probes {
            let ctx = format!("{:?} from line {from_line}", String::from_utf8_lossy(bytes));
            let mut term = codex();
            term.vi_toggle();
            place(&mut term, from_line, 1);
            term.process(bytes);
            assert_eq!(term.vi_cursor_point(), ViPoint::new(to_line, 1), "{ctx}");
            assert_eq!(text(&term, to_line), on, "{ctx}");
        }
    }
}

/// An interior region (a pane below another, a list between a header and a
/// footer) scrolls with no history. A vi cursor on one of its rows follows its
/// text; one whose text left the region rests on the edge it left through;
/// one outside the region stays.
#[test]
fn an_interior_region_scroll_carries_the_vi_cursor_on_its_rows_only() {
    // A region over rows 1..=3 between a header and two footer rows.
    fn pane() -> Terminal {
        let mut term = Terminal::new(6, 20);
        term.process(b"head\r\np1\r\np2\r\np3\r\nfoot1\r\nfoot2\x1b[2;4r");
        term.vi_toggle();
        term
    }
    // A vi line before the bytes, after them, and the text it is on after, if
    // it kept its text.
    type Move = (i32, i32, Option<&'static str>);
    let cases: [(&[u8], [Move; 5]); 2] = [
        // A line fed at the region's bottom: its rows go up one, `p1` leaves.
        (
            b"\x1b[4;1H\r\nq1",
            [
                (0, 0, Some("head")),
                (1, 1, None),
                (2, 1, Some("p2")),
                (3, 2, Some("p3")),
                (4, 4, Some("foot1")),
            ],
        ),
        // A reverse index at its top: its rows go down one, `p3` leaves.
        (
            b"\x1b[2;1H\x1bM",
            [
                (0, 0, Some("head")),
                (1, 2, Some("p1")),
                (2, 3, Some("p2")),
                (3, 3, None),
                (4, 4, Some("foot1")),
            ],
        ),
    ];
    for (bytes, moves) in cases {
        for (from_line, to_line, on) in moves {
            let ctx = format!("{:?} from line {from_line}", String::from_utf8_lossy(bytes));
            let mut term = pane();
            place(&mut term, from_line, 1);
            term.process(bytes);
            assert_eq!(term.vi_cursor_point(), ViPoint::new(to_line, 1), "{ctx}");
            if let Some(on) = on {
                assert_eq!(text(&term, to_line), on, "{ctx}");
            }
        }
    }
}

/// A rows-only resize relabels every line: a shrink pushes the top rows into
/// history, a grow brings them back. The vi cursor keeps its text through
/// both, as the selection does.
#[test]
fn a_rows_resize_keeps_the_vi_cursor_on_its_text() {
    for (line, on) in [(1, "a3"), (5, "a7")] {
        let mut term = Terminal::new(6, 20);
        term.process(b"a2\r\na3\r\na4\r\na5\r\na6\r\na7");
        term.vi_toggle();
        place(&mut term, line, 1);
        assert_eq!(text(&term, line), on);
        term.resize(3, 20);
        assert_eq!(
            term.grid().scrollback_lines(),
            3,
            "the shrink demoted three rows"
        );
        assert_eq!(
            term.vi_cursor_point(),
            ViPoint::new(line - 3, 1),
            "shrunk, on {on:?}"
        );
        assert_eq!(text(&term, line - 3), on, "shrunk");
        term.resize(6, 20);
        assert_eq!(
            term.vi_cursor_point(),
            ViPoint::new(line, 1),
            "grown back, on {on:?}"
        );
        assert_eq!(text(&term, line), on, "grown back");
    }
}

/// A width shrink rewraps every line, so nothing maps the vi cursor onto its
/// text; it is clamped onto the grid, its column inside the new width.
#[test]
fn a_width_shrink_clamps_the_vi_cursor_onto_the_grid() {
    let mut term = Terminal::new(4, 20);
    term.process(b"one\r\ntwo\r\nthree");
    term.vi_toggle();
    place(&mut term, 2, 15);
    term.resize(4, 8);
    let at = term.vi_cursor_point();
    let floor = -i32::try_from(term.grid().scrollback_lines()).expect("small history");
    assert!(
        (floor..4).contains(&at.line),
        "line {} on the grid",
        at.line
    );
    assert_eq!(at.col, 7, "the column inside the new width");
}

/// The window's width change hands the history to a reflow worker
/// (`resize_offloading_scrollback`): no history is in memory while it runs, so
/// a cursor in history keeps its line until the history is back, and stays on
/// its text. Clamped at the resize, it landed on live line 0.
#[test]
fn an_offloaded_width_change_keeps_the_vi_cursor_in_history_on_its_text() {
    let mut term = Terminal::with_scrollback(
        10,
        40,
        8,
        aterm_core::scrollback::Scrollback::new(64, 512, 8_000_000),
    );
    for i in 0..200 {
        term.process(format!("L{i}\r\n").as_bytes());
    }
    term.vi_toggle();
    place(&mut term, -45, 1);
    let before = text(&term, -45);
    assert!(
        before.starts_with('L'),
        "fixture: a line in history ({before:?})"
    );
    let mut job = term.resize_offloading_scrollback(10, 60);
    assert!(job.is_some(), "fixture: the reflow is offloaded");
    while let Some(pending) = job {
        job = term.finish_resize_offload(pending.reflow());
    }
    assert_eq!(term.vi_cursor_point(), ViPoint::new(-45, 1));
    assert_eq!(text(&term, -45), before, "on its text");
}

/// A cleared history (`clear`'s ED 3, or the host's clear) takes the line a vi
/// cursor in history was on: it is clamped onto the screen. A lowered history
/// limit takes the oldest lines: a cursor on one rests on the oldest line kept.
#[test]
fn a_cleared_history_clamps_the_vi_cursor_onto_the_screen() {
    for host in [false, true] {
        let mut term = Terminal::new(ROWS, COLS);
        term.process(LINES.join("\r\n").as_bytes());
        term.vi_toggle();
        place(&mut term, -2, 1);
        if host {
            term.clear_scrollback();
        } else {
            term.process(b"\x1b[H\x1b[2J\x1b[3J");
        }
        assert_eq!(
            term.grid().scrollback_lines(),
            0,
            "history cleared (host {host})"
        );
        assert_eq!(term.vi_cursor_point(), ViPoint::new(0, 1), "host {host}");
    }
    let mut term = Terminal::new(ROWS, COLS);
    term.process(LINES.join("\r\n").as_bytes());
    term.vi_toggle();
    place(&mut term, -HISTORY, 1);
    term.set_scrollback_line_limit(Some(1));
    assert_eq!(term.grid().scrollback_lines(), 1, "history cut to one line");
    assert_eq!(term.vi_cursor_point(), ViPoint::new(-1, 1));
    assert_eq!(text(&term, -1), LINES[2]);
}

/// On the alternate screen a rows shrink demotes the top rows into the grid's
/// resize undo, not into history, so a vi cursor on one rests on the top row.
/// A grow that hands the undo back puts it back on its text, as it puts the
/// selection back. A cursor moved after the undo's first shrink has no
/// pre-image there: it rides the relabels, through a second shrink too.
#[test]
fn a_resize_flap_on_the_alt_screen_puts_the_vi_cursor_back_on_its_text() {
    // Where the cursor starts, the shrinks, a motion pressed after the first,
    // the text the cursor is on after the first shrink and once grown back.
    type Flap = (
        i32,
        &'static [u16],
        Option<ViMotion>,
        &'static str,
        &'static str,
    );
    let cases: [Flap; 3] = [
        // Its row was demoted: on the top row, then back on its text.
        (1, &[3], None, "a4", "a2"),
        // Moved after the shrink.
        (1, &[3], Some(ViMotion::Down), "a4", "a5"),
        // Moved between two shrinks of one undo.
        (4, &[4, 3], Some(ViMotion::Up), "a5", "a4"),
    ];
    for (start, shrinks, motion, shrunk, on) in cases {
        let ctx = format!("from line {start}, shrunk to {shrinks:?}, then {motion:?}");
        let mut term = Terminal::new(6, 20);
        term.process(b"\x1b[?1049h\x1b[Ha1\r\na2\r\na3\r\na4\r\na5\r\na6");
        term.vi_toggle();
        place(&mut term, start, 1);
        for (i, &rows) in shrinks.iter().enumerate() {
            term.resize(rows, 20);
            if i == 0 {
                let at = term.vi_cursor_point();
                assert_eq!(text(&term, at.line), shrunk, "{ctx}");
                if let Some(motion) = motion {
                    press(&mut term, motion);
                }
            }
        }
        assert_eq!(
            term.grid().scrollback_lines(),
            0,
            "the alt screen keeps no history"
        );
        term.resize(6, 20);
        let at = term.vi_cursor_point();
        assert_eq!(at.col, 1, "{ctx}");
        assert_eq!(text(&term, at.line), on, "{ctx}");
    }
}
