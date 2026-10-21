// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! A rows-only flap of the primary screen with the program's output between
//! its halves (aterm-grid `grid/resize_handback.rs`, ATERM-KI-1015-1 S2R-F4),
//! fed through the parser as the program wrote it and judged by what a
//! terminal fed the same bytes shows when it never resized, or when it stayed
//! at the final height.

use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::Terminal;

fn screen(term: &Terminal) -> Vec<String> {
    (0..usize::from(term.rows()))
        .map(|r| term.row_text(r).unwrap_or_default().trim_end().to_string())
        .collect()
}

fn offloaded_resize(term: &mut Terminal, rows: u16, cols: u16) {
    let mut job = term.resize_offloading_scrollback(rows, cols);
    while let Some(pending) = job {
        job = term.finish_resize_offload(pending.reflow());
    }
}

/// How a test resizes: the synchronous `Terminal::resize`, or the GUI's
/// offloaded one (a rows-only resize is the plain one there).
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

/// A plain shell in a fresh tab of the window's shape (144 columns, tiered
/// history): a prompt and six lines at the top of 50 rows, the window shrunk
/// to 10, a twenty-line command run there, then the window grown part of the
/// way back (12, 30 or 40 rows) or all of it. The screen is the one a terminal
/// that stayed at the final height shows.
#[test]
fn a_shell_grown_part_of_the_way_back_shows_the_screen_that_stayed() {
    let feed = |term: &mut Terminal, part: u8| {
        if part == 0 {
            term.process(b"$ ls\r\n");
            for i in 0..5 {
                term.process(format!("p{i}\r\n").as_bytes());
            }
        } else {
            term.process(b"$ seq 20\r\n");
            for i in 0..20 {
                term.process(format!("o{i}\r\n").as_bytes());
            }
        }
        term.process(b"$ ");
    };
    for path in [Path::Sync, Path::Offloaded] {
        for target in [12u16, 30, 40, 50] {
            let mut term = Terminal::with_scrollback(50, 144, 1000, Scrollback::with_defaults());
            feed(&mut term, 0);
            path.resize(&mut term, 10, 144);
            feed(&mut term, 1);
            path.resize(&mut term, target, 144);
            let mut stayed =
                Terminal::with_scrollback(target, 144, 1000, Scrollback::with_defaults());
            feed(&mut stayed, 0);
            feed(&mut stayed, 1);
            let at = format!("{path:?}: 50 -> 10, seq 20, -> {target}");
            assert_eq!(screen(&term), screen(&stayed), "{at}");
            assert_eq!(term.cursor(), stayed.cursor(), "{at}");
        }
    }
}

/// Eight rows with history above them, the screen cleared and `top` drawn on
/// its first row, the cursor under it.
fn cleared() -> Terminal {
    let mut term = Terminal::new(8, 20);
    for i in 0..20 {
        term.process(format!("h{i}\r\n").as_bytes());
    }
    term.process(b"\x1b[H\x1b[2Jtop\r\n");
    assert!(
        term.grid().scrollback_lines() > 0,
        "history above the screen"
    );
    term
}

/// A program that puts its cursor on the revealed history lines after a grow
/// (CUP or HVP home, VPA to the first row, CUU past the top) draws
/// top-anchored on the taller screen without erasing it (each line erases
/// only its own tail, EL, so no cell of the row under it is left to tell the
/// screens apart). The shrink back trims under its lines, as on a screen that
/// never grew: every row shows what a terminal that never resized shows, and
/// the cursor is on the same row. A cursor that stays on the rows the program
/// drew keeps them owed (aterm-grid `Grid::note_cursor_moved_up`).
#[test]
fn a_program_that_moves_onto_the_revealed_rows_keeps_its_lines_through_the_shrink() {
    // (the grow, the program's bytes)
    let shapes: [(u16, &[u8]); 5] = [
        (9, b"\x1b[HN0\x1b[K\r\nN1\x1b[K\r\nN2\x1b[K\r\n"),
        (10, b"\x1b[HN0\x1b[K\r\nN1\x1b[K\r\nN2\x1b[K\r\n"),
        (9, b"\x1b[1;1fN0\x1b[K\r\nN1\x1b[K\r\nN2\x1b[K\r\n"),
        (9, b"\r\x1b[1dN0\x1b[K\r\nN1\x1b[K\r\nN2\x1b[K\r\n"),
        (9, b"\x1b[99A\rN0\x1b[K\r\nN1\x1b[K\r\nN2\x1b[K\r\n"),
    ];
    for (grow, bytes) in shapes {
        let at = format!("grow to {grow}, {:?}", String::from_utf8_lossy(bytes));
        let mut never = cleared();
        let mut flapped = cleared();
        flapped.resize(grow, 20);
        never.process(bytes);
        flapped.process(bytes);
        assert_eq!(
            flapped.grid().revealed_rows(),
            0,
            "{at}: drawn from the top"
        );
        flapped.resize(8, 20);
        assert_eq!(screen(&flapped), screen(&never), "{at}");
        assert_eq!(flapped.cursor(), never.cursor(), "{at}");
    }

    // A reverse index onto the revealed line is the program there too.
    let mut term = cleared();
    term.resize(9, 20);
    assert_eq!(term.grid().revealed_rows(), 1);
    term.process(b"\r\x1bM\x1bM");
    assert_eq!(term.cursor().row, 0);
    assert_eq!(term.grid().revealed_rows(), 0, "RI onto the revealed line");
}

/// The control: a program that moves relative to its own lines (a prompt's
/// redraw, an inline renderer walking up its frame) never reaches the revealed
/// history line, so the shrink still demotes it back and every row is where a
/// terminal that never resized shows it.
#[test]
fn a_relative_redraw_after_a_grow_keeps_the_hand_back() {
    for bytes in [
        &b"\x1b[AX\r\n"[..],
        b"more\r\n",
        b"\x1b[A\x1b[2K\rredrawn\r\n",
    ] {
        let at = format!("{:?}", String::from_utf8_lossy(bytes));
        let mut never = cleared();
        let mut flapped = cleared();
        flapped.resize(9, 20);
        never.process(bytes);
        flapped.process(bytes);
        assert_eq!(flapped.grid().revealed_rows(), 1, "{at}: still owed");
        flapped.resize(8, 20);
        assert_eq!(screen(&flapped), screen(&never), "{at}");
        assert_eq!(flapped.cursor(), never.cursor(), "{at}");
    }
}
