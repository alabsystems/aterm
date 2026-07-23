// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Selection highlight correctness while scrolled into scrollback.
//!
//! The selection model stores anchor rows TERMINAL-relative (0 = top of the
//! live screen, negative = scrollback). The renderer converts each DISPLAY row
//! `r` to a terminal row via `r - display_offset` before calling
//! `contains_cell`, and `selection_to_string` reads terminal-relative rows
//! directly. This pins that contract: a selection over scrollback lines, made
//! while `display_offset > 0`, both (a) copies the scrollback text and (b)
//! highlights the matching DISPLAY rows — not the live grid rows.

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;

#[test]
fn selection_over_scrollback_highlights_correct_display_rows() {
    // 5-row screen so it is easy to reason about display vs scrollback.
    let mut term = Terminal::new(5, 20);
    for i in 0..20 {
        term.process(format!("line{i}\r\n").as_bytes());
    }

    // Scroll up 4 lines: the viewport top now shows a scrollback line.
    term.scroll_display(4);
    let display_offset = term.grid().display_offset() as i32;
    assert_eq!(display_offset, 4, "scrolled 4 lines into history");

    // Select display rows 0..=1 (top two visible rows, which are scrollback).
    // The caller convention is to pass TERMINAL-relative rows; for display row
    // `d` that is `d - display_offset`.
    let term_start = 0 - display_offset; // display row 0 -> terminal row -4
    let term_end = 1 - display_offset; // display row 1 -> terminal row -3
    {
        let sel = term.text_selection_mut();
        sel.start_selection(term_start, 0, SelectionSide::Left, SelectionType::Lines);
        sel.update_selection(term_end, 19, SelectionSide::Right);
        sel.complete_selection();
    }

    // (a) Copied text must be the SCROLLBACK lines shown at display rows 0..=1,
    // NOT live grid rows 0..=1. With display_offset=4, display row 0 shows the
    // line that is `display_row_text(0)`.
    let copied = term.selection_to_string().expect("selection has text");
    let want_row0 = term.display_row_text(0).expect("display row 0 text");
    let want_row1 = term.display_row_text(1).expect("display row 1 text");
    assert_eq!(copied, format!("{want_row0}\n{want_row1}"));

    // (b) Renderer convention: for each DISPLAY row r, the highlight test is
    // contains_cell(r - display_offset, ...). The selected display rows (0,1)
    // must report contained; rows below must not.
    let sel = term.text_selection();
    let contained = |display_row: i32, col: u16| {
        sel.contains_cell(display_row - display_offset, col, false, false)
    };
    assert!(contained(0, 0), "display row 0 is highlighted");
    assert!(contained(1, 10), "display row 1 is highlighted");
    assert!(!contained(2, 0), "display row 2 is NOT highlighted");
    assert!(!contained(4, 0), "bottom display row is NOT highlighted");
}

/// Regression (round 18): a pathological out-of-range selection — e.g. injected
/// via the control-socket `select` verb, whose row args are parsed as unclamped
/// i32 — must NOT spin `selection_to_string` over ~2^31 rows (each pushing a
/// newline) while the caller holds the terminal lock. That was a repeatable
/// UI-freeze DoS. The loop is now bounded to the grid's valid coordinate range;
/// this test would hang / OOM before the fix and completes promptly after it.
#[test]
fn selection_to_string_bounds_pathological_out_of_range_span() {
    let mut term = Terminal::new(5, 20);
    term.process(b"hello");

    {
        let sel = term.text_selection_mut();
        // Anchor from (0,0) down to a row far past the bottom of the grid.
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(i32::MAX, 0, SelectionSide::Right);
        sel.complete_selection();
    }

    // Bounded to the 5 visible rows — returns immediately instead of spinning.
    let copied = term.selection_to_string().expect("selection has text");
    let line_count = copied.split('\n').count();
    assert!(
        line_count <= 5,
        "selection_to_string must be bounded to the grid rows, got {line_count} lines"
    );
    assert!(copied.starts_with("hello"), "row 0 content is preserved");
}
