// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A width change keeps the text the program wrote, on the screen and in every
//! history tier, through the synchronous resize and the GUI's offloaded one.
//!
//! Two losses this file pins (bug hunt 9, text fidelity):
//!
//! * The early-wrap hole. A width-2 glyph that cannot start in the last column
//!   wraps early and leaves that cell unwritten. Every rewrap seam used to infer
//!   the hole from the next row ("it opens with a wide cell") and dropped the
//!   last cell of any such row, so a space or a glyph the program really wrote
//!   there was deleted for good. The hole is now recorded when it is made (the
//!   write path and the rewrap chunker mark the row, a history line simply
//!   leaves it out) and only a recorded hole is dropped, as Ghostty drops only a
//!   `spacer_head` and Alacritty only a `LEADING_WIDE_CHAR_SPACER`.
//! * Reflow-copied blanks dropped out of `Row::len`, so a printed space at a
//!   chunk end stopped being content: tiered history (staged at `len`) joined
//!   words on the next width change, and a cursor after a printed space was
//!   clamped onto it.

use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::Terminal;

/// Every logical line, oldest history first, then the screen: history lines
/// joined on their wrapped flag, screen rows (their content, `row_text`) joined
/// the same way. A wrapped row's trailing blanks beyond its content are not
/// read, which is exact for the round trips below: each ends at a width where
/// the line fits on one row.
fn logical_lines(term: &Terminal) -> Vec<String> {
    let grid = term.grid();
    let mut out: Vec<String> = Vec::new();
    for i in 0..grid.scrollback_lines() {
        let line = grid.get_history_line(i).expect("history line");
        let text = line.to_string();
        if line.is_wrapped() && !out.is_empty() {
            out.last_mut().expect("non-empty").push_str(&text);
        } else {
            out.push(text);
        }
    }
    for r in 0..grid.rows() {
        let text = grid.row_text(r).expect("visible row");
        let wrapped = grid.row(r).is_some_and(|row| row.is_wrapped());
        if wrapped && !out.is_empty() {
            out.last_mut().expect("non-empty").push_str(&text);
        } else {
            out.push(text);
        }
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

/// Every screen row's text, exactly.
fn screen(term: &Terminal) -> Vec<String> {
    let grid = term.grid();
    (0..grid.rows())
        .map(|r| grid.row_text(r).expect("visible row"))
        .collect()
}

/// Drive the GUI's offloaded width change to completion.
fn offloaded_resize(term: &mut Terminal, rows: u16, cols: u16) {
    let mut job = term.resize_offloading_scrollback(rows, cols);
    while let Some(pending) = job {
        job = term.finish_resize_offload(pending.reflow());
    }
}

/// A tiered terminal as the window builds one, with a small ring so history
/// reaches the store.
fn tiered(rows: u16, cols: u16, ring: usize) -> Terminal {
    Terminal::with_scrollback(rows, cols, ring, Scrollback::with_defaults())
}

/// On screen: a real space in the last column before a wide glyph survives a
/// narrow-then-widen round trip, exactly (the finding's inputs, and its ZWJ
/// family line, whose every space between the clusters survives).
#[test]
fn a_real_space_before_an_early_wrapped_glyph_survives_a_screen_round_trip() {
    for (text, narrow) in [
        ("abc 中fg", 4u16),
        ("done ✅ ok", 5),
        ("가나다 라마", 7),
        ("abcd 中文", 5),
        ("👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 zwj", 9),
    ] {
        let mut term = Terminal::new(6, 20);
        term.process(text.as_bytes());
        let before = screen(&term);
        term.resize(6, narrow);
        term.resize(6, 20);
        assert_eq!(screen(&term), before, "{text:?} through {narrow} columns");
    }
}

/// A recorded hole is still dropped, and only it: a glyph that really
/// early-wrapped rejoins its line with no phantom space, also when the hole was
/// blanked with a coloured background (BCE), and the narrowing back restores
/// the early wrap.
#[test]
fn a_real_early_wrap_hole_is_dropped_on_rejoin() {
    let hole_bg = |frame: &aterm_core::render::RenderInput| {
        frame.cells[0].get(9).map_or_else(
            || {
                let [_, r, g, b] = frame.default_bg.to_be_bytes();
                [r, g, b]
            },
            |cell| cell.bg,
        )
    };
    for text in [
        "abcdefghi中xyz",
        "\x1b[41mabcdefghi中\x1b[0mxyz",
        "\x1b[48;2;17;34;51mabcdefghi中\x1b[0mxyz",
    ] {
        let mut term = Terminal::new(6, 10);
        term.process(text.as_bytes());
        let before = term.cell_frame(6, 10);
        let before_bg = hole_bg(&before);
        assert!(
            term.grid().row(1).is_some_and(|r| r.is_wrapped()),
            "fixture: the wide glyph early-wrapped ({text:?})"
        );
        term.resize(6, 20);
        assert_eq!(
            logical_lines(&term),
            vec!["abcdefghi中xyz".to_string()],
            "{text:?}"
        );
        term.resize(6, 10);
        let after = screen(&term);
        assert_eq!(
            after[0].trim_end(),
            "abcdefghi",
            "{text:?}: the hole is blank"
        );
        assert_eq!(after[1], "中xyz", "{text:?}: the early wrap comes back");
        let after = term.cell_frame(6, 10);
        assert_eq!(
            hole_bg(&after),
            before_bg,
            "{text:?}: the regenerated hole uses the wrapping glyph's background"
        );
    }
}

/// A rectangle moves the provenance of its last cell as well as its pixels.
/// A copied printed blank must not inherit the destination's old hole marker:
/// reflow would delete that real space when the next row continues it.
#[test]
fn margined_vertical_moves_keep_printed_blanks_and_wrap_holes_distinct() {
    for (name, op, source, destination) in [
        ("IL", "\x1b[L", 0, 1),
        ("DL", "\x1b[M", 1, 0),
        ("SU", "\x1b[S", 1, 0),
        ("SD", "\x1b[T", 0, 1),
    ] {
        let mut term = Terminal::new(6, 4);
        term.process(format!("\x1b[{};1Habc中", destination + 1).as_bytes());
        assert!(term.grid().row(destination).unwrap().has_early_wrap_hole());
        term.process(format!("\x1b[{};1Hdef ", source + 1).as_bytes());
        term.process(b"\x1b[3;1Hgh");
        term.process(format!("\x1b[?69h\x1b[2;4s\x1b[1;2H{op}").as_bytes());
        assert!(
            !term.grid().row(destination).unwrap().has_early_wrap_hole(),
            "{name}: the incoming printed blank is content"
        );
        term.resize(6, 8);
        let lines = logical_lines(&term);
        assert!(
            lines.iter().any(|line| line.contains("aef ")),
            "{name}: reflow deleted the copied printed space: {lines:?}"
        );

        // Conversely, a copied hole remains unwritten. A rectangle ending
        // before the last column leaves a destination hole where it stood.
        for (right, expected_row) in [(4, destination), (3, source)] {
            let mut term = Terminal::new(6, 4);
            term.process(format!("\x1b[{};1Habc中", source + 1).as_bytes());
            term.process(format!("\x1b[?69h\x1b[2;{right}s\x1b[1;2H{op}").as_bytes());
            assert!(
                term.grid().row(expected_row).unwrap().has_early_wrap_hole(),
                "{name}: right={right}, hole belongs to row {expected_row}"
            );
        }
    }
}

/// The written line, the history fixture: written at `cols` on a 3-row screen
/// and scrolled into history by five line feeds.
fn in_history(mut term: Terminal, text: &str) -> Terminal {
    term.process(text.as_bytes());
    term.process(b"\r\n\r\n\r\n\r\n\r\n");
    assert!(term.grid().scrollback_lines() >= 2, "fixture: in history");
    term
}

/// In history, every tier, both resize paths: a full row's last cell (a space
/// or a glyph) before a wide glyph survives a widening, and a narrowing back
/// restores the original rows.
#[test]
fn a_full_rows_last_cell_survives_widening_out_of_history() {
    let cases: [(&str, u16, &str); 4] = [
        ("abc 中fg", 4, "abc 中fg"),
        ("hello你好", 5, "hello你好"),
        ("abcd中fg", 4, "abcd中fg"),
        ("abcdefghij中xyz", 10, "abcdefghij中xyz"),
    ];
    for (text, cols, want) in cases {
        type Make = fn(u16) -> Terminal;
        let makes: [(&str, Make); 2] = [
            ("ring", |cols| Terminal::new(3, cols)),
            ("tiered", |cols| tiered(3, cols, 1)),
        ];
        for (tier, make) in makes {
            for offload in [false, true] {
                let mut term = in_history(make(cols), text);
                if offload {
                    offloaded_resize(&mut term, 3, 20);
                } else {
                    term.resize(3, 20);
                }
                let lines = logical_lines(&term);
                assert!(
                    lines.iter().any(|l| l == want),
                    "{text:?} at {cols} in {tier} history, offload={offload}: {lines:?}"
                );
                if offload {
                    offloaded_resize(&mut term, 3, cols);
                } else {
                    term.resize(3, cols);
                }
                let lines = logical_lines(&term);
                assert!(
                    lines.iter().any(|l| l == want),
                    "{text:?} back at {cols} in {tier} history, offload={offload}: {lines:?}"
                );
            }
        }
    }
}

/// A hole the rewrap itself makes (a chunk backed off a wide glyph) is
/// recorded like one the write path makes, on the screen and on history rows
/// rebuilt from rewrapped lines, so the next rewrap drops it and nothing else.
#[test]
fn a_hole_a_rewrap_made_is_dropped_by_the_next_rewrap() {
    const LINE: &str = "abcdefghi中xyz";
    // On screen: no hole at 20 columns, one at 10, none at 20 again.
    let mut term = Terminal::new(6, 20);
    term.process(LINE.as_bytes());
    term.resize(6, 10);
    term.resize(6, 20);
    assert_eq!(logical_lines(&term), vec![LINE.to_string()], "screen");
    // In ring history: the narrowing rebuilds the history rows from rewrapped
    // lines; the widening reads them back.
    for offload in [false, true] {
        let mut term = in_history(tiered(3, 20, 100), LINE);
        for cols in [10, 20] {
            if offload {
                offloaded_resize(&mut term, 3, cols);
            } else {
                term.resize(3, cols);
            }
        }
        let lines = logical_lines(&term);
        assert!(
            lines.iter().any(|l| l == LINE),
            "ring history, offload={offload}: {lines:?}"
        );
    }
}

/// A recorded hole is dropped from history too, in every tier and on both
/// paths, also when the wrap blanked it with a background colour: the row's
/// length then covers the hole, and a line staged at that length would carry a
/// phantom coloured space into the next rewrap.
#[test]
fn a_coloured_early_wrap_hole_stays_out_of_history() {
    type Make = fn(u16) -> Terminal;
    let makes: [(&str, Make); 2] = [
        ("ring", |cols| Terminal::new(3, cols)),
        ("tiered", |cols| tiered(3, cols, 1)),
    ];
    for (tier, make) in makes {
        for offload in [false, true] {
            let mut term = in_history(make(10), "\x1b[41mabcdefghi中\x1b[0mxyz");
            if offload {
                offloaded_resize(&mut term, 3, 20);
            } else {
                term.resize(3, 20);
            }
            let lines = logical_lines(&term);
            assert!(
                lines.iter().any(|l| l == "abcdefghi中xyz"),
                "{tier} history, offload={offload}: {lines:?}"
            );
        }
    }
}

/// Read without a rewrap, the coloured hole is no text either, as its staged
/// line already leaves it out: on screen (a copy and the row text) and in either
/// history tier, the line reads the same as with the default background.
#[test]
fn a_coloured_early_wrap_hole_is_no_text_before_any_rewrap() {
    use aterm_core::selection::{SelectionSide, SelectionType};
    let text = "\x1b[44mabcdefghi中\x1b[0mxyz";
    let mut term = Terminal::new(4, 10);
    term.process(text.as_bytes());
    assert!(
        logical_lines(&term).iter().any(|l| l == "abcdefghi中xyz"),
        "on screen: {:?}",
        logical_lines(&term)
    );
    let sel = term.text_selection_mut();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(1, 9, SelectionSide::Right);
    sel.complete_selection();
    assert_eq!(
        term.selection_to_string().as_deref(),
        Some("abcdefghi中xyz"),
        "a copy on screen"
    );
    type Make = fn(u16) -> Terminal;
    let makes: [(&str, Make); 2] = [
        ("ring", |cols| Terminal::new(3, cols)),
        ("tiered", |cols| tiered(3, cols, 1)),
    ];
    for (tier, make) in makes {
        let term = in_history(make(10), text);
        let lines = logical_lines(&term);
        assert!(
            lines.iter().any(|l| l == "abcdefghi中xyz"),
            "{tier} history: {lines:?}"
        );
    }
}

/// A hole is a hole until something is written into it: a program that later
/// prints a space in that last cell (a line editor redrawing its line) made
/// the space content, and a rewrap keeps it.
#[test]
fn a_space_written_into_a_hole_later_is_kept() {
    let mut term = Terminal::new(6, 10);
    term.process("abcdefghi中xyz".as_bytes());
    // Back to the hole (row 1, column 10) and print a space into it.
    term.process(b"\x1b[1;10H \x1b[2;4H");
    term.resize(6, 20);
    assert_eq!(logical_lines(&term), vec!["abcdefghi 中xyz".to_string()]);
}

/// An edit that rewrites the hole's column other than by erasing it ends the
/// hole, and the blank it leaves there is a cell the screen shows inside its
/// line: ICH shifts a printed space into the column, DCH shifts the hole left
/// and fills the column, a wide glyph over the column (alone, or the end of a
/// run) whose head a narrow glyph then overwrites leaves its spacer's blank,
/// and a DECFRA glyph there that DECERA then erases leaves an erased blank. A
/// rewrap keeps that blank.
#[test]
fn an_edit_over_the_holes_column_ends_the_hole() {
    for (text, edit, expected) in [
        ("ab 中xy", &b"\x1b[1;1H\x1b[@"[..], " ab 中xy"),
        ("abc中xy", b"\x1b[1;1H\x1b[P", "bc  中xy"),
        ("abc中xy", "\x1b[1;3H文\x1b[1;3Hx".as_bytes(), "abx 中xy"),
        ("abc中xy", "\x1b[1;1H文文\x1b[1;3Hx".as_bytes(), "文x 中xy"),
        // DECFRA `x` into the hole's column, then DECERA erases it (ECH or EL
        // there would end the soft wrap itself).
        ("abc中xy", b"\x1b[120;1;4;1;4$x\x1b[1;4;1;4$z", "abc 中xy"),
    ] {
        let mut term = Terminal::new(6, 4);
        term.process(text.as_bytes());
        assert!(
            term.grid().row(1).is_some_and(|r| r.is_wrapped()),
            "fixture: the wide glyph early-wrapped ({text:?})"
        );
        term.process(edit);
        term.resize(6, 20);
        assert_eq!(
            logical_lines(&term),
            vec![expected.to_string()],
            "{text:?} edited by {edit:?}"
        );
    }
}

/// A line whose head is in history and whose tail rows lead the screen (the
/// boundary lift) keeps every full row's last cell: the lifted row before the
/// wide-led row, and the history head before the lifted rows.
#[test]
fn a_line_straddling_the_history_boundary_keeps_its_full_rows_last_cells() {
    const LINE: &str = "0123456789abcdefghij中xyz";
    for offload in [false, true] {
        let mut term = tiered(4, 10, 4);
        // `0123456789` scrolls into history; `abcdefghij` and `中xyz` lead the
        // screen as continuations above the cursor.
        term.process(format!("{LINE}\r\n$ \r\n").as_bytes());
        assert!(
            term.grid().row(1).is_some_and(|r| r.is_wrapped()),
            "fixture: the tail leads the screen"
        );
        let lines = logical_lines(&term);
        assert!(lines.iter().any(|l| l == LINE), "fixture {lines:?}");
        if offload {
            offloaded_resize(&mut term, 4, 30);
        } else {
            term.resize(4, 30);
        }
        let lines = logical_lines(&term);
        assert!(
            lines.iter().any(|l| l == LINE),
            "offload={offload}: {lines:?}"
        );
    }
}

/// Tiered history keeps a printed space at a reflow chunk end: two width
/// changes do not join the words around it (synchronous path: the narrowing
/// pushes rewrapped rows straight into the store).
#[test]
fn tiered_history_keeps_spaces_at_reflow_chunk_ends() {
    let lines = [
        "the quick brown fox jumps over the lazy dog",
        "pack my box with five dozen liquor jugs",
        "sphinx of black quartz judge my vow",
        "how vexingly quick daft zebras jump",
    ];
    let mut term = tiered(6, 20, 1000);
    for line in lines {
        term.process(format!("{line}\r\n").as_bytes());
    }
    term.resize(6, 10);
    term.resize(6, 20);
    assert_eq!(logical_lines(&term), lines.to_vec());
}

/// The same through the GUI's offloaded path, with the reflowed ring spilling
/// into the tiered store before the second width change.
#[test]
fn offloaded_tiered_history_keeps_spaces_at_reflow_chunk_ends() {
    let lines = [
        "the quick brown fox jumps over the lazy dog",
        "sphinx of black quartz judge my vow",
    ];
    let mut term = tiered(6, 20, 64);
    for line in lines {
        term.process(format!("{line}\r\n").as_bytes());
    }
    offloaded_resize(&mut term, 6, 10);
    for i in 0..74 {
        term.process(format!("n{i}\r\n").as_bytes());
    }
    offloaded_resize(&mut term, 6, 20);
    let got = logical_lines(&term);
    for line in lines {
        assert!(got.iter().any(|l| l == line), "{line:?} in {got:?}");
    }
}

/// A run of printed spaces wider than a chunk survives the ring spill.
#[test]
fn a_long_run_of_printed_spaces_survives_offloaded_width_changes() {
    let line = format!("ab{}cd", " ".repeat(60));
    let mut term = Terminal::with_scrollback(10, 40, 8, Scrollback::with_defaults());
    term.process(format!("{line}\r\n").as_bytes());
    offloaded_resize(&mut term, 10, 25);
    // Enough output to push the rewrapped rows out of the 8-row ring.
    for i in 0..30 {
        term.process(format!("l{i}\r\n").as_bytes());
    }
    offloaded_resize(&mut term, 10, 33);
    offloaded_resize(&mut term, 10, 80);
    let got = logical_lines(&term);
    assert!(got.contains(&line), "{got:?}");
}

/// A prompt ending in a printed space (`read -p`, nothing redraws it) keeps the
/// cursor after the space through two width changes, so typed input does not
/// overwrite it.
#[test]
fn a_cursor_after_a_printed_space_keeps_its_column() {
    for widths in [[10u16, 20], [30, 40]] {
        let mut term = Terminal::new(4, 20);
        term.process(b"Name: ");
        for cols in widths {
            term.resize(4, cols);
        }
        term.process(b"Bob");
        assert_eq!(
            term.grid().row_text(0).as_deref(),
            Some("Name: Bob"),
            "through {widths:?}"
        );
    }
}

/// A seamless handoff's checkpoint carries each screen row as history reads
/// one, so an engine restored from it rewraps exactly as the live one: an
/// early-wrap hole after cells a tab, CUF or CHA skipped stays a hole (read
/// at its length it came back unmarked, and the next widening added a space),
/// and a soft-wrapped row DCH left one short of the margin keeps its blank
/// (read at its length it came back with a hole guessed, and the widening
/// dropped the blank). Sync and offloaded, through both restore paths.
#[test]
fn a_restored_engine_rewraps_as_the_live_one() {
    for input in [
        "abc\t\t中xy",
        "abc\x1b[10G中xy",
        "abc\x1b[9C中xy",
        "abcdefghijklm\x1b[1;5H\x1b[P",
    ] {
        for offload in [false, true] {
            let build = || {
                if offload {
                    tiered(4, 10, 100)
                } else {
                    Terminal::new(4, 10)
                }
            };
            let mut live = build();
            live.process(input.as_bytes());
            let cp = live.checkpoint_carry(0).expect("Ground");
            let mut fresh = Terminal::from_checkpoint(&cp);
            let mut adopted = build();
            adopted.restore_checkpoint(&cp);
            let mut widened = Vec::new();
            for term in [&mut live, &mut fresh, &mut adopted] {
                if offload {
                    offloaded_resize(term, 4, 20);
                } else {
                    term.resize(4, 20);
                }
                widened.push(screen(term));
            }
            assert_eq!(
                widened[0], widened[1],
                "from_checkpoint {input:?} offload={offload}"
            );
            assert_eq!(
                widened[0], widened[2],
                "restore_checkpoint {input:?} offload={offload}"
            );
        }
    }
}

/// A history row rebuilt at a new width keeps an OSC 8 link or SGR 58
/// underline colour only on the cells the rebuild put a glyph back in.
///
/// One column cannot hold a wide glyph: the rewrap gives it a line of its own,
/// which rebuilds as an empty ring row (`fill_row_into` places nothing), as
/// the screen's own rewrap blanks it (`copy_cells_to_row`). Its link and
/// underline colour came back anyway, keyed to the empty cell, and a rows grow
/// re-seated them there: an extras entry no cell claimed (which a later
/// extras-styled write adopts), or, where the reveal claims what it re-seats,
/// a click target on an empty cell. Found 2026-10-11 (r16) checking every
/// path that shrinks a wide glyph out of existence, after the alternate
/// screen's width cut; the synchronous ring path and the window's offloaded
/// tiered path both rebuild ring rows this way.
#[test]
fn a_wide_glyph_one_column_cannot_hold_leaves_no_link_in_history() {
    let link_on = "\x1b]8;;https://example.com/\x1b\\";
    let link_off = "\x1b]8;;\x1b\\";
    for glyph in ["\u{1F680}", "\u{4E2D}"] {
        let line = format!("\x1b[4:3m\x1b[58;5;1m{link_on}{glyph}{link_off}\x1b[m\r\n");
        for offload in [false, true] {
            let mut term = if offload {
                tiered(3, 4, 2)
            } else {
                let mut term = Terminal::new(3, 4);
                term.set_scrollback_line_limit(Some(100));
                term
            };
            for _ in 0..6 {
                term.process(line.as_bytes());
            }
            let what = format!("{glyph:?} offload={offload}");
            assert!(term.hyperlink_at(0, 0).is_some(), "{what}: linked before");
            let history = term.grid().scrollback_lines();
            for (rows, cols) in [(3, 1), (12, 1)] {
                if offload {
                    offloaded_resize(&mut term, rows, cols);
                } else {
                    term.resize(rows, cols);
                }
            }
            assert!(
                term.grid().scrollback_lines() < history,
                "{what}: the rows grow reveals history"
            );
            for row in 0..term.rows() {
                assert_eq!(
                    term.hyperlink_at(row, 0),
                    None,
                    "{what}: row {row} holds no glyph, so it links nothing"
                );
            }
            let (rows, cols) = (term.rows(), term.cols());
            for (at, extra) in term.grid().extras().iter() {
                assert!(
                    at.row >= rows
                        || at.col >= cols
                        || (extra.hyperlink().is_none() && extra.underline_color_index().is_none()),
                    "{what}: ({}, {}) keeps the dropped glyph's {extra:?}",
                    at.row,
                    at.col
                );
            }
        }
    }
}

/// Every cell of the view, at every display offset, whose link or SGR 58
/// underline colour disagrees with `styled` (whether the cell holds part of
/// the glyph the program styled, read from the cell's own char and, for a
/// wide continuation, its lead's). The view is read as a person scrolls it:
/// the screen, then each history row through its tier's materialization.
fn view_style_strays(
    term: &mut Terminal,
    styled: impl Fn(char) -> bool,
    what: &str,
) -> Vec<String> {
    let (rows, cols) = (term.rows(), term.cols());
    let history = term.grid().scrollback_lines();
    let mut strays = Vec::new();
    term.scroll_to_bottom();
    for offset in 0..=history {
        if offset > 0 {
            term.scroll_display(1);
        }
        for r in 0..rows {
            let view = term.grid().visible_row_view(r);
            for c in 0..cols {
                let ch = view.col_char(c);
                let lead = if ch == '\0' && c > 0 {
                    view.col_char(c - 1)
                } else {
                    ch
                };
                let want = styled(lead);
                let link = term.hyperlink_at_visible(r, c).is_some();
                let underline = view.cell(c).is_some_and(|cell| {
                    view.cell_data(c, cell)
                        .cell_extra()
                        .is_some_and(|extra| extra.underline_color_u32().is_some())
                });
                if link != want || underline != want {
                    strays.push(format!(
                        "{what}: offset {offset} ({r}, {c}) {lead:?} link={link} underline={underline}"
                    ));
                }
            }
        }
    }
    term.scroll_to_bottom();
    strays
}

/// Scrolled back, a history row rebuilt where a wide glyph cannot fit shows
/// no link or underline colour on its empty cell.
///
/// The fills that rebuild ring rows already restore a line's spans only on
/// the cells they filled (`filled_span_cols`). The scrolled-back view of a
/// tiered line goes through its own walk (`materialize_from_line`), which
/// restored them up to the row's width: on one-column history every empty
/// row of a dropped glyph answered its link. Found 2026-10-11 by the r16
/// alt-extras review (48 linked blanks over 16 tiered one-column cases).
#[test]
fn a_one_column_history_row_scrolled_back_styles_no_blank() {
    let link_on = "\x1b]8;;https://example.com/\x1b\\";
    let link_off = "\x1b]8;;\x1b\\";
    for glyph in ["\u{1F680}", "\u{4E2D}"] {
        let line = format!("\x1b[4:3m\x1b[58;5;1m{link_on}{glyph}{link_off}\x1b[m\r\n");
        for offload in [false, true] {
            let mut term = if offload {
                tiered(3, 4, 2)
            } else {
                let mut term = Terminal::new(3, 4);
                term.set_scrollback_line_limit(Some(100));
                term
            };
            for _ in 0..6 {
                term.process(line.as_bytes());
            }
            if offload {
                offloaded_resize(&mut term, 3, 1);
            } else {
                term.resize(3, 1);
            }
            let what = format!("{glyph:?} offload={offload}");
            assert!(
                term.grid().scrollback_lines() >= 3,
                "{what}: history to scroll back through"
            );
            // One column holds none of the glyph, so nothing is styled.
            let strays = view_style_strays(&mut term, |_| false, &what);
            assert!(strays.is_empty(), "{strays:#?}");
        }
    }
}

/// A VS16 glyph the live grid kept narrow in mid row keeps the columns after
/// it, in scrollback and through a history rewrap, so the link and underline
/// colour of the glyph after it stay on that glyph.
///
/// The live writer cannot widen `❤️` at the last column (no room for the
/// spacer), and the screen's own rewrap keeps that narrow cell narrow when a
/// width change moves it into mid row. The line it scrolls into history
/// records it narrow and keys the spans after it to those columns. Every
/// reader re-widened it from its text: the scrolled-back row drew it two
/// columns wide, a blank took the link of the 🚀 after it and the 🚀's
/// right half lost it, and the history rewrap did the same to the line it
/// built. Found 2026-10-11 by the r16 alt-extras review (3x4, widen to 3x8,
/// scroll, 12x9).
#[test]
fn a_vs16_glyph_kept_narrow_in_mid_row_keeps_the_styles_after_it_in_place() {
    let text = "abc\u{2764}\u{FE0F} \x1b[4:3m\x1b[58;5;1m\x1b]8;;https://e.x/\x1b\\\u{1F680}\x1b]8;;\x1b\\\x1b[m";
    let rocket = |ch: char| ch == '\u{1F680}';
    for offload in [false, true] {
        let mut term = if offload {
            tiered(3, 4, 2)
        } else {
            let mut term = Terminal::new(3, 4);
            term.set_scrollback_line_limit(Some(100));
            term
        };
        let resize = |term: &mut Terminal, rows: u16, cols: u16| {
            if offload {
                offloaded_resize(term, rows, cols);
            } else {
                term.resize(rows, cols);
            }
        };
        term.process(text.as_bytes());
        resize(&mut term, 3, 8);
        let what = format!("offload={offload}");
        let row = term.grid().visible_row_view(0);
        let cells: String = (0..8).map(|c| row.col_char(c)).collect();
        assert_eq!(
            cells, "abc\u{2764} \u{1F680}\0 ",
            "{what}: the screen keeps the heart narrow in mid row"
        );
        let strays = view_style_strays(&mut term, rocket, &format!("{what} on screen"));
        assert!(strays.is_empty(), "{strays:#?}");
        // Into the ring, then on into the tiered store.
        term.process(b"\r\n\r\n\r\n\r\n");
        let strays = view_style_strays(&mut term, rocket, &format!("{what} in history"));
        assert!(strays.is_empty(), "{strays:#?}");
        for _ in 0..8 {
            term.process(b"x\r\n");
        }
        let strays = view_style_strays(&mut term, rocket, &format!("{what} deeper"));
        assert!(strays.is_empty(), "{strays:#?}");
        // The history rewrap builds its line from the same columns.
        resize(&mut term, 12, 9);
        let strays = view_style_strays(&mut term, rocket, &format!("{what} rewrapped"));
        assert!(strays.is_empty(), "{strays:#?}");
    }
}
