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
/// narrow-then-widen round trip, exactly (the finding's three inputs).
#[test]
fn a_real_space_before_an_early_wrapped_glyph_survives_a_screen_round_trip() {
    for (text, narrow) in [
        ("abc 中fg", 4u16),
        ("done ✅ ok", 5),
        ("가나다 라마", 7),
        ("abcd 中文", 5),
    ] {
        let mut term = Terminal::new(6, 20);
        term.process(text.as_bytes());
        let before = screen(&term);
        term.resize(6, narrow);
        term.resize(6, 20);
        assert_eq!(screen(&term), before, "{text:?} through {narrow} columns");
    }
}

/// The ZWJ family line from the finding: every space between the clusters
/// survives a 30 -> 9 -> 30 round trip.
#[test]
fn spaces_between_emoji_clusters_survive_a_screen_round_trip() {
    let text = "👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 zwj";
    let mut term = Terminal::new(6, 30);
    term.process(text.as_bytes());
    let before = screen(&term);
    term.resize(6, 9);
    term.resize(6, 30);
    assert_eq!(screen(&term), before);
}

/// A recorded hole is still dropped, and only it: a glyph that really
/// early-wrapped rejoins its line with no phantom space, also when the hole was
/// blanked with a coloured background (BCE), and the narrowing back restores
/// the early wrap.
#[test]
fn a_real_early_wrap_hole_is_dropped_on_rejoin() {
    for text in ["abcdefghi中xyz", "\x1b[41mabcdefghi中\x1b[0mxyz"] {
        let mut term = Terminal::new(6, 10);
        term.process(text.as_bytes());
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
        assert_eq!(
            screen(&term)[..2],
            ["abcdefghi".to_string(), "中xyz".to_string()],
            "{text:?}: the early wrap comes back"
        );
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
