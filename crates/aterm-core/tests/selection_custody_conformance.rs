// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Selection custody: damage is a question about overlap.

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;

fn with_scrollback_selection(rows: u16, cols: u16) -> Terminal {
    let mut term = Terminal::new(rows, cols);
    for i in 0..64 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    term.scroll_to_top();
    assert_ne!(term.grid().display_offset(), 0, "scrolled back");
    let sel = term.text_selection_mut();
    sel.start_selection(-30, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(-30, 9, SelectionSide::Right);
    sel.complete_selection();
    assert!(term.text_selection().has_selection());
    term
}

fn selected_text(term: &Terminal) -> String {
    term.selection_to_string()
        .expect("a live selection must resolve to text")
}

#[test]
fn a_region_scroll_clears_a_selection_inside_it() {
    let mut term = Terminal::new(24, 40);
    for i in 0..64 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    {
        let sel = term.text_selection_mut();
        sel.start_selection(20, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(20, 4, SelectionSide::Right);
        sel.complete_selection();
    }
    assert!(term.text_selection().has_selection());

    term.process(b"\x1b[19;24r");
    term.process(b"\x1b[24;1H\n");

    assert!(
        !term.text_selection().has_selection(),
        "a selection inside the scrolled region must be cleared"
    );

    {
        let sel = term.text_selection_mut();
        sel.start_selection(2, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(2, 9, SelectionSide::Right);
        sel.complete_selection();
    }
    let before = selected_text(&term);
    term.process(b"\x1b[24;1H\n");
    assert!(
        term.text_selection().has_selection(),
        "the SAME region scroll must spare a selection above the region"
    );
    assert_eq!(
        selected_text(&term),
        before,
        "…and must not remap it: rows outside the region did not move"
    );
}

#[test]
fn a_reverse_region_scroll_spares_a_scrollback_selection() {
    let mut term = with_scrollback_selection(24, 40);
    let before = selected_text(&term);

    term.process(b"\x1b[19;24r");
    term.process(b"\x1b[19;1H\x1bM");

    assert!(
        term.text_selection().has_selection(),
        "a reverse region scroll must not reach into scrollback"
    );
    assert_eq!(
        selected_text(&term),
        before,
        "…and must not remap it either"
    );
}

#[test]
fn ordinary_output_still_preserves_a_scrollback_selection() {
    let mut term = with_scrollback_selection(24, 40);
    let before = selected_text(&term);

    for i in 0..5 {
        term.process(format!("more-{i}\r\n").as_bytes());
    }

    assert!(
        term.text_selection().has_selection(),
        "a uniform scroll must keep a scrollback selection"
    );
    assert_eq!(
        selected_text(&term),
        before,
        "the selection must ride the content, not sit still while it scrolls"
    );
}

#[test]
fn clearing_the_scrollback_still_clears_the_selection() {
    let mut term = with_scrollback_selection(24, 40);
    let before = selected_text(&term);

    term.process(b"\x1b[2J");
    assert!(
        term.text_selection().has_selection(),
        "ED 2 touches the visible screen only; a scrollback selection is not its business"
    );
    assert_eq!(selected_text(&term), before);

    term.process(b"\x1b[3J");

    assert!(
        !term.text_selection().has_selection(),
        "ED 3 destroys the coordinate space; the selection must go with it"
    );
}

#[test]
fn erasing_another_line_spares_the_highlight() {
    let mut term = Terminal::new(24, 40);
    for i in 0..5 {
        term.process(format!("row-{i}\r\n").as_bytes());
    }
    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 4, SelectionSide::Right);
        sel.complete_selection();
    }

    let before = selected_text(&term);

    term.process(b"\x1b[4;1H\x1b[K");

    assert!(
        term.text_selection().has_selection(),
        "EL on an unrelated row must not clear the selection"
    );
    assert_eq!(selected_text(&term), before, "…and must not move it");

    term.process(b"\x1b[1;1H\x1b[K");
    assert!(
        !term.text_selection().has_selection(),
        "the same EL, aimed at the selected row, must clear the stale highlight"
    );
}

#[test]
fn changing_attributes_over_the_selection_spares_it() {
    let mut term = Terminal::new(24, 40);
    term.process(b"hello world\r\n");
    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 4, SelectionSide::Right);
        sel.complete_selection();
    }

    let before = selected_text(&term);

    term.process(b"\x1b[1;1;2;10;1$r");

    assert!(
        term.text_selection().has_selection(),
        "an attribute-only change rewrites no text; the highlight still names it"
    );
    assert_eq!(
        selected_text(&term),
        before,
        "…and the copy is byte-identical, which is the reason it may be spared"
    );

    term.process(b"\x1b[2;1;3;10;1;1;1$v");
    assert!(
        !term.text_selection().has_selection(),
        "replacing the characters under the highlight is not an attribute change"
    );
}

#[test]
fn a_rect_copy_clears_over_its_destination_but_spares_its_source() {
    let mut term = Terminal::new(24, 40);
    for i in 0..10 {
        term.process(format!("row-{i}\r\n").as_bytes());
    }
    {
        let sel = term.text_selection_mut();
        sel.start_selection(5, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(5, 4, SelectionSide::Right);
        sel.complete_selection();
    }
    term.process(b"\x1b[1;1;2;10;1;6;1$v");
    assert!(
        !term.text_selection().has_selection(),
        "the destination rows were overwritten; the highlight is stale"
    );

    let mut term = Terminal::new(24, 40);
    for i in 0..20 {
        term.process(format!("row-{i}\r\n").as_bytes());
    }
    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 4, SelectionSide::Right);
        sel.complete_selection();
    }
    let before = selected_text(&term);
    term.process(b"\x1b[1;1;2;10;1;15;1$v");
    assert!(
        term.text_selection().has_selection(),
        "reading the selected rows as a copy SOURCE must not clear the highlight"
    );
    assert_eq!(
        selected_text(&term),
        before,
        "…and the source cells are unchanged, so the copy is too"
    );
}

#[test]
fn a_rows_grow_that_reveals_history_moves_the_selection_with_its_content() {
    let mut term = Terminal::new(4, 20);
    for i in 0..40 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    {
        let sel = term.text_selection_mut();
        sel.start_selection(1, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(1, 6, SelectionSide::Right);
        sel.complete_selection();
    }
    let before = term
        .selection_to_string()
        .expect("the selection resolves to text");
    assert!(!before.is_empty());

    term.resize(10, 20);

    assert!(
        term.text_selection().has_selection(),
        "a rows-only GROW must not destroy the selection"
    );
    let after = term
        .selection_to_string()
        .expect("the surviving selection still resolves");
    assert_eq!(
        after, before,
        "the selection must still name the SAME text after a rows-grow that \
         revealed history; anchors that stay put while content moves down are a \
         wrong-copy path"
    );
}

fn with_capped_history(rows: u16, cols: u16, limit: usize) -> Terminal {
    let mut term = Terminal::new(rows, cols);
    term.set_scrollback_line_limit(Some(limit));
    for i in 0..(limit + usize::from(rows) + 20) {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    assert_eq!(
        term.grid().scrollback_lines(),
        limit,
        "the history cap must actually be saturated"
    );
    term
}

#[test]
fn evicting_one_endpoint_truncates_instead_of_destroying_the_selection() {
    let mut term = with_capped_history(24, 40, 50);
    {
        let sel = term.text_selection_mut();
        sel.start_selection(-50, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(-46, 6, SelectionSide::Right);
        sel.complete_selection();
    }
    let before = term
        .selection_to_string()
        .expect("the five-row span resolves to text");
    assert!(!term.text_selection().truncated(), "nothing lost yet");

    term.process(b"more-0\r\n");
    term.process(b"more-1\r\n");

    assert!(
        term.text_selection().has_selection(),
        "the retained half of the span must survive its evicted half"
    );
    assert_eq!(term.text_selection().normalized_start().row, -50);
    assert_eq!(
        term.text_selection().normalized_start().col,
        0,
        "the clamped head starts at the beginning of the oldest retained row"
    );
    assert_eq!(term.text_selection().normalized_end().row, -48);
    assert!(term.text_selection().truncated());

    let (text, incomplete) = term.selection_to_string_bounded();
    let text = text.expect("the surviving span still resolves to text");
    assert!(
        incomplete,
        "the caps did not fire, so this flag can only be the eviction — it is the \
         one signal that stops a short answer reading as an exact one"
    );
    assert!(
        before.ends_with(&text),
        "what survives is the TAIL of what was selected, unchanged: {before:?} vs {text:?}"
    );
}

#[test]
fn evicting_both_endpoints_still_clears_the_selection() {
    let mut term = with_capped_history(24, 40, 50);
    {
        let sel = term.text_selection_mut();
        sel.start_selection(-50, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(-48, 6, SelectionSide::Right);
        sel.complete_selection();
    }
    for i in 0..4 {
        term.process(format!("more-{i}\r\n").as_bytes());
    }
    assert!(!term.text_selection().has_selection());
    assert!(!term.text_selection().truncated());
    assert_eq!(term.selection_to_string_bounded(), (None, false));
}

#[test]
fn shrinking_the_retention_limit_truncates_a_selection_it_evicted() {
    let mut term = with_capped_history(24, 40, 50);
    {
        let sel = term.text_selection_mut();
        sel.start_selection(-50, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(-46, 6, SelectionSide::Right);
        sel.complete_selection();
    }

    term.set_scrollback_line_limit(Some(48));
    assert_eq!(term.grid().scrollback_lines(), 48);

    assert!(term.text_selection().has_selection());
    assert_eq!(
        term.text_selection().normalized_start().row,
        -48,
        "the head clamps onto the new floor with no scroll to carry it"
    );
    assert_eq!(
        term.text_selection().normalized_end().row,
        -46,
        "the retained endpoint does not move: eviction renumbers nothing"
    );
    assert!(term.text_selection().truncated());
}

#[test]
fn a_scroll_on_the_alt_screen_clears_rather_than_clamping_to_alt_row_zero() {
    let mut term = Terminal::new(24, 40);
    for i in 0..64 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    term.process(b"\x1b[?1049h");
    for i in 0..10 {
        term.process(format!("alt-{i}\r\n").as_bytes());
    }
    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 3, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(2, 7, SelectionSide::Right);
        sel.complete_selection();
    }
    assert!(term.text_selection().has_selection());

    term.process(b"\x1b[24;1H\n");

    assert!(
        !term.text_selection().has_selection(),
        "alt has no history: the evicted row is unrecoverable and row 0 is not a \
         substitute for it"
    );
    assert!(!term.text_selection().truncated());
}

fn with_live_row_zero_selection() -> Terminal {
    let mut term = Terminal::new(24, 40);
    term.process(b"hello world\r\n");
    term.process(b"\x1b[1;1H");
    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 4, SelectionSide::Right);
        sel.complete_selection();
    }
    assert!(term.text_selection().has_selection());
    term
}

#[test]
fn char_edits_on_another_line_spare_the_highlight() {
    for op in [&b"\x1b[40X"[..], &b"\x1b[3P"[..], &b"\x1b[3@"[..]] {
        let mut term = with_live_row_zero_selection();

        term.process(b"\x1b[4;1H");
        term.process(op);

        assert!(
            term.text_selection().has_selection(),
            "a char edit on an unrelated row must not clear the selection: {op:?}"
        );
    }
}

#[test]
fn char_edits_on_the_selected_line_clear_the_stale_highlight() {
    for op in [&b"\x1b[40X"[..], &b"\x1b[3P"[..], &b"\x1b[3@"[..]] {
        let mut term = with_live_row_zero_selection();

        term.process(b"\x1b[1;1H");
        term.process(op);

        assert!(
            !term.text_selection().has_selection(),
            "a char edit ON the selected row must clear the stale highlight: {op:?}"
        );
    }
}

#[test]
fn every_print_path_records_output_damage_on_the_row_it_lands() {
    for (setup, overwrite) in [
        ("", "\rAAAA"),             // bulk ASCII blast
        ("", "\ré"),                // one non-ASCII char: the per-character path
        ("", "\réé"),               // bulk Unicode run
        ("X", "\x1b[1;1H\x1b[10b"), // REP repeats the setup batch's 'X'
    ] {
        let mut term = Terminal::new(24, 40);
        term.process(format!("Progress: 10%{setup}").as_bytes());
        {
            let sel = term.text_selection_mut();
            sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            sel.update_selection(0, 12, SelectionSide::Right);
            sel.complete_selection();
        }
        assert!(term.text_selection().has_selection());

        term.process(overwrite.as_bytes());

        assert!(
            !term.text_selection().has_selection(),
            "print path {overwrite:?} rewrote the selected row without recording it"
        );
    }
}

#[test]
fn ordinary_output_on_another_row_spares_the_highlight() {
    let mut term = Terminal::new(24, 40);
    term.process(b"hello world\x1b[10;1Hbackground job output");
    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 4, SelectionSide::Right);
        sel.complete_selection();
    }
    assert!(term.text_selection().has_selection());

    term.process(b"\x1b[10;1Hbackground job OUTPUT");

    assert!(
        term.text_selection().has_selection(),
        "an overwrite on an unrelated row must not clear the selection"
    );
}

#[test]
fn appending_to_the_selected_row_spares_the_highlight() {
    let mut term = Terminal::new(24, 40);
    term.process(b"hello");
    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 4, SelectionSide::Right);
        sel.complete_selection();
    }
    assert!(term.text_selection().has_selection());

    term.process(b" world");

    assert!(
        term.text_selection().has_selection(),
        "appending past the row's content replaces nothing the highlight names"
    );
    assert_eq!(
        term.selection_to_string().as_deref(),
        Some("hello"),
        "and the copy still returns exactly what was selected"
    );
}

#[test]
fn a_fifth_disjoint_repaint_still_spares_the_gap_between_them() {
    let mut term = Terminal::new(24, 40);
    {
        let sel = term.text_selection_mut();
        sel.start_selection(2, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(2, 4, SelectionSide::Right);
        sel.complete_selection();
    }
    assert!(
        term.text_selection().has_selection(),
        "precondition: the selection exists before any repaint"
    );

    term.process(b"\x1b[1;1H\x1b[K\x1b[5;1H\x1b[K\x1b[9;1H\x1b[K\x1b[13;1H\x1b[K\x1b[17;1H\x1b[K");

    assert!(
        term.text_selection().has_selection(),
        "five disjoint EL repaints are still five bands; row 2 lies in a gap"
    );
}

#[test]
fn past_the_bound_a_selection_on_a_repainted_row_still_clears() {
    let mut term = Terminal::new(24, 40);
    {
        let sel = term.text_selection_mut();
        sel.start_selection(4, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(4, 4, SelectionSide::Right);
        sel.complete_selection();
    }
    assert!(term.text_selection().has_selection(), "precondition");

    term.process(
        b"\x1b[1;1H\x1b[K\x1b[3;1H\x1b[K\x1b[5;1H\x1b[K\x1b[7;1H\x1b[K\x1b[9;1H\x1b[K\
          \x1b[11;1H\x1b[K\x1b[13;1H\x1b[K\x1b[15;1H\x1b[K\x1b[17;1H\x1b[K",
    );

    assert!(
        !term.text_selection().has_selection(),
        "the selected row was erased; coverage is the safety property the bound keeps"
    );
}

#[test]
fn the_output_dedup_guard_does_not_leak_into_the_next_batch() {
    let mut term = Terminal::new(24, 40);
    term.process(b"\x1b[6;1Horiginal content here");

    let select_row_5 = |term: &mut Terminal| {
        let sel = term.text_selection_mut();
        sel.start_selection(5, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(5, 8, SelectionSide::Right);
        sel.complete_selection();
    };

    select_row_5(&mut term);
    term.process(b"\x1b[6;1Hfirst overwrite");
    assert!(
        !term.text_selection().has_selection(),
        "precondition: an overwriting print clears a selection on the row it replaced"
    );

    select_row_5(&mut term);
    assert!(
        term.text_selection().has_selection(),
        "precondition: re-selected"
    );
    term.process(b"\x1b[6;1Hsecond overwrite");
    assert!(
        !term.text_selection().has_selection(),
        "the guard belongs to the drained batch; a second batch overwriting the same \
         row must record its own damage"
    );
}
