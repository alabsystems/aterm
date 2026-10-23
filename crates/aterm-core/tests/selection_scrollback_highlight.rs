// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Selection text and highlight mapping across scrollback.

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;

#[test]
fn selection_over_scrollback_highlights_correct_display_rows() {
    let mut term = Terminal::new(5, 20);
    for i in 0..20 {
        term.process(format!("line{i}\r\n").as_bytes());
    }

    term.scroll_display(4);
    let display_offset = term.grid().display_offset() as i32;
    assert_eq!(display_offset, 4, "scrolled 4 lines into history");

    // Select display rows 0..=1 via terminal-relative coordinates (`d - display_offset`).
    let term_start = 0 - display_offset; // display row 0 -> terminal row -4
    let term_end = 1 - display_offset; // display row 1 -> terminal row -3
    {
        let sel = term.text_selection_mut();
        sel.start_selection(term_start, 0, SelectionSide::Left, SelectionType::Lines);
        sel.update_selection(term_end, 19, SelectionSide::Right);
        sel.complete_selection();
    }

    let copied = term.selection_to_string().expect("selection has text");
    let want_row0 = term.display_row_text(0).expect("display row 0 text");
    let want_row1 = term.display_row_text(1).expect("display row 1 text");
    assert_eq!(copied, format!("{want_row0}\n{want_row1}"));

    let sel = term.text_selection();
    let contained = |display_row: i32, col: u16| {
        sel.contains_cell(display_row - display_offset, col, false, false)
    };
    assert!(contained(0, 0), "display row 0 is highlighted");
    assert!(contained(1, 10), "display row 1 is highlighted");
    assert!(!contained(2, 0), "display row 2 is NOT highlighted");
    assert!(!contained(4, 0), "bottom display row is NOT highlighted");
}

/// Out-of-range selection coordinates clamp to the grid's row range.
#[test]
fn selection_to_string_bounds_pathological_out_of_range_span() {
    let mut term = Terminal::new(5, 20);
    term.process(b"hello");

    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(i32::MAX, 0, SelectionSide::Right);
        sel.complete_selection();
    }

    let copied = term.selection_to_string().expect("selection has text");
    let line_count = copied.split('\n').count();
    assert!(
        line_count <= 5,
        "selection_to_string must be bounded to the grid rows, got {line_count} lines"
    );
    assert!(copied.starts_with("hello"), "row 0 content is preserved");
}
