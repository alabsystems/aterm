// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! What is highlighted is what is copied — at both edges, over wide glyphs.
//!
//! A double-width glyph (CJK, emoji, a ZWJ sequence) owns a lead cell and a
//! blank continuation cell, and the two ends of a selection are independent
//! half-cell anchors, so a drag can stop INSIDE one. The renderer decides per
//! cell (`RenderInput::selection_contains_cell` -> `TextSelection::contains_cell`)
//! and the clipboard decides per column range (`Terminal::selection_to_string`).
//! Two authorities over one boundary is how the highlight comes to cover half a
//! character while the clipboard receives the whole one — literally
//! not-what-you-see-is-what-you-get. [`glyph_cell_span`] is the one rule both
//! consult, and these tests are what holds them to it.
//!
//! These tests reconstruct the painted run through the renderer's OWN frame and
//! predicate and compare it to the copy, so the two cannot drift apart without
//! one of them failing here.
//!
//! [`glyph_cell_span`]: aterm_types::selection::glyph_cell_span

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;

/// The columns the RENDERER would fill on `row`, read through the exact input
/// and predicate it paints with: a real frame's per-cell `wide` bit fed to
/// `selection_contains_cell`, the one seam both the CPU and GPU backends use.
fn painted_cols(term: &mut Terminal, rows: usize, cols: usize, row: usize) -> Vec<usize> {
    let frame = term.cell_frame(rows, cols);
    let row_cells = &frame.cells[row];
    (0..cols)
        .filter(|&c| {
            // Exactly the renderer's spelling of the two width bits: a cell is a
            // wide LEAD when the cell after it is a continuation.
            let is_wide_lead = row_cells.get(c + 1).is_some_and(|n| n.wide);
            let is_continuation = row_cells.get(c).is_some_and(|n| n.wide);
            frame.selection_contains_cell(row, c, is_wide_lead, is_continuation)
        })
        .collect()
}

/// The text under the painted run on `row` — each highlighted cell's grapheme,
/// in reading order, trailing blanks trimmed the way the copy trims them.
///
/// `display_cell_grapheme` because `row` is a DISPLAY row, like the frame's:
/// identical to the live read while unscrolled, and the only correct one once a
/// selection edge sits on a history line.
fn painted_text(term: &mut Terminal, rows: usize, cols: usize, row: usize) -> String {
    let cols_painted = painted_cols(term, rows, cols, row);
    let mut out = String::new();
    for c in cols_painted {
        out.push_str(&term.display_cell_grapheme(row, c).unwrap_or_default());
    }
    out.trim_end().to_string()
}

/// Every glyph is painted whole or not at all: a highlight that covers one half
/// of a double-width cell pair and not the other is a band that ends inside a
/// character, which no copy can reproduce.
fn assert_no_glyph_is_split(term: &mut Terminal, rows: usize, cols: usize, row: usize) {
    let painted = painted_cols(term, rows, cols, row);
    let frame = term.cell_frame(rows, cols);
    let row_cells = &frame.cells[row];
    for c in 0..cols {
        let is_continuation = row_cells.get(c).is_some_and(|n| n.wide);
        if !is_continuation || c == 0 {
            continue;
        }
        assert_eq!(
            painted.contains(&(c - 1)),
            painted.contains(&c),
            "the glyph at cols {}-{c} is painted in half — the highlight ends \
             inside a character the clipboard can only take whole (painted: {painted:?})",
            c - 1
        );
    }
}

/// Drive one selection and assert the painted run and the copied run are the
/// same run, plus the no-split invariant.
fn assert_paint_and_copy_agree(
    term: &mut Terminal,
    rows: usize,
    cols: usize,
    row: usize,
    what: &str,
) {
    assert_no_glyph_is_split(term, rows, cols, row);
    let painted = painted_text(term, rows, cols, row);
    let copied = term.selection_to_string().unwrap_or_default();
    assert_eq!(
        painted, copied,
        "{what}: the highlight covers {painted:?} but the clipboard receives {copied:?}"
    );
}

/// Lay out `AB漢字CD`: A=0 B=1 漢=2-3 字=4-5 C=6 D=7.
fn cjk_row() -> Terminal {
    let mut term = Terminal::new(3, 20);
    term.process("AB漢字CD".as_bytes());
    term
}

#[test]
fn a_selection_starting_on_a_wide_glyphs_right_half_copies_what_it_paints() {
    let mut term = cjk_row();
    {
        let sel = term.text_selection_mut();
        // The drag begins on col 3 — the blank right half of 漢.
        sel.start_selection(0, 3, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 6, SelectionSide::Right);
        sel.complete_selection();
    }
    assert_paint_and_copy_agree(&mut term, 3, 20, 0, "start on a wide continuation");
    // Which way they agree is itself the contract: the glyph the edge landed
    // inside joins the SELECTION whole, rather than being dropped from the copy.
    assert_eq!(term.selection_to_string().as_deref(), Some("漢字C"));
    assert_eq!(painted_cols(&mut term, 3, 20, 0), vec![2, 3, 4, 5, 6]);
}

#[test]
fn a_selection_ending_on_a_wide_glyphs_left_half_copies_what_it_paints() {
    let mut term = cjk_row();
    {
        let sel = term.text_selection_mut();
        // The drag stops on col 2 — the lead of 漢, its right half untouched.
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 2, SelectionSide::Right);
        sel.complete_selection();
    }
    assert_paint_and_copy_agree(&mut term, 3, 20, 0, "end on a wide lead");
    assert_eq!(term.selection_to_string().as_deref(), Some("AB漢"));
    assert_eq!(painted_cols(&mut term, 3, 20, 0), vec![0, 1, 2, 3]);
}

#[test]
fn a_drag_that_halts_between_the_halves_of_a_glyph_agrees_on_both_sides() {
    // The two half-cell anchors that land BETWEEN 漢's cells: an end on the left
    // side of the continuation, and a start on the right side of the lead. Both
    // are what a real pointer produces when it stops over the glyph's middle.
    let mut term = cjk_row();
    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 3, SelectionSide::Left);
        sel.complete_selection();
    }
    assert_paint_and_copy_agree(&mut term, 3, 20, 0, "end at the glyph's midpoint");

    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 2, SelectionSide::Right, SelectionType::Simple);
        sel.update_selection(0, 6, SelectionSide::Right);
        sel.complete_selection();
    }
    assert_paint_and_copy_agree(&mut term, 3, 20, 0, "start at the glyph's midpoint");
}

#[test]
fn a_backwards_drag_over_a_wide_glyph_agrees_exactly_as_a_forwards_one_does() {
    let mut term = cjk_row();
    {
        let sel = term.text_selection_mut();
        // Same two edges, anchored in the opposite order (pointer moving left).
        sel.start_selection(0, 6, SelectionSide::Right, SelectionType::Simple);
        sel.update_selection(0, 3, SelectionSide::Left);
        sel.complete_selection();
    }
    assert_paint_and_copy_agree(&mut term, 3, 20, 0, "backwards over a wide continuation");
    assert_eq!(term.selection_to_string().as_deref(), Some("漢字C"));

    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 2, SelectionSide::Right, SelectionType::Simple);
        sel.update_selection(0, 0, SelectionSide::Left);
        sel.complete_selection();
    }
    assert_paint_and_copy_agree(&mut term, 3, 20, 0, "backwards ending at a wide lead");
    // The same span the forward drag produced: anchor ORDER is normalized away
    // before the edges widen, so the direction of the pointer never shows up in
    // either answer.
    assert_eq!(term.selection_to_string().as_deref(), Some("AB漢"));
    assert_eq!(painted_cols(&mut term, 3, 20, 0), vec![0, 1, 2, 3]);
}

#[test]
fn a_zwj_emoji_is_selected_whole_from_either_of_its_halves() {
    // A ZWJ sequence is ONE cluster two cells wide, so it splits exactly the way
    // a CJK ideograph does — and, unlike a CJK char, it is many code points, so
    // half a highlight over it could never be described by half its text.
    let mut term = Terminal::new(3, 20);
    term.process("A👩\u{200d}💻Z".as_bytes()); // A=0, the emoji=1-2, Z=3

    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 2, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 3, SelectionSide::Right);
        sel.complete_selection();
    }
    assert_paint_and_copy_agree(&mut term, 3, 20, 0, "start on the emoji's continuation");
    assert_eq!(term.selection_to_string().as_deref(), Some("👩\u{200d}💻Z"));

    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 1, SelectionSide::Right);
        sel.complete_selection();
    }
    assert_paint_and_copy_agree(&mut term, 3, 20, 0, "end on the emoji's lead");
    assert_eq!(term.selection_to_string().as_deref(), Some("A👩\u{200d}💻"));
}

#[test]
fn a_combining_cluster_occupies_one_cell_and_no_edge_can_divide_it() {
    // The width-1 control: an NFD accent is several code points in ONE cell, so
    // there is no boundary inside it for an anchor to land on. Pinned alongside
    // the wide cases so the snapping rule is not mistaken for a cluster rule —
    // it is a CELL-PAIR rule, and this row must be unaffected by it.
    let mut term = Terminal::new(3, 20);
    term.process("Ae\u{301}Z".as_bytes()); // A=0, e+U+0301=1, Z=2

    for (start, end, want) in [
        (0u16, 1u16, "Ae\u{301}"),
        (1, 1, "e\u{301}"),
        (1, 2, "e\u{301}Z"),
    ] {
        {
            let sel = term.text_selection_mut();
            sel.start_selection(0, start, SelectionSide::Left, SelectionType::Simple);
            sel.update_selection(0, end, SelectionSide::Right);
            sel.complete_selection();
        }
        assert_paint_and_copy_agree(&mut term, 3, 20, 0, "over a combining cluster");
        assert_eq!(term.selection_to_string().as_deref(), Some(want));
    }
}

#[test]
fn a_block_selection_over_a_wide_glyph_agrees_the_same_way_a_linear_one_does() {
    // A rectangular drag reaches the same rule through the same helper, so the
    // agreement is a property of the glyph, not of the selection kind: from
    // either half, the whole glyph.
    let mut term = cjk_row();
    for (start, end) in [(3u16, 6u16), (0, 2)] {
        {
            let sel = term.text_selection_mut();
            sel.start_selection(0, start, SelectionSide::Left, SelectionType::Block);
            sel.update_selection(0, end, SelectionSide::Right);
            sel.complete_selection();
        }
        assert_paint_and_copy_agree(&mut term, 3, 20, 0, "block over a wide glyph");
    }
}

#[test]
fn a_selection_whose_edge_is_a_wide_glyph_in_scrollback_agrees_there_too() {
    // History rows take a different extraction path entirely (a byte-offset walk
    // over the stored line, not a per-cell walk over the grid), so the agreement
    // has to hold on both. Scroll the wide row off-screen, then select its right
    // half from within scrollback and compare against the painted display row.
    let mut term = Terminal::new(3, 20);
    term.process("AB漢字CD\r\n".as_bytes());
    for i in 0..4 {
        term.process(format!("filler{i}\r\n").as_bytes());
    }
    // Scroll back until the wide row is the top display row.
    let mut offset = 0i32;
    let wide_display_row = loop {
        let found = (0..3).find(|&r| {
            term.display_row_text(r)
                .is_some_and(|t| t.starts_with("AB漢"))
        });
        if let Some(r) = found {
            break r;
        }
        assert!(offset < 8, "the wide row should still be in history");
        term.scroll_display(1);
        offset += 1;
    };
    let display_offset = i32::try_from(term.grid().display_offset()).expect("small offset");
    assert!(
        display_offset > 0,
        "the wide row is genuinely in scrollback"
    );
    let sel_row = i32::try_from(wide_display_row).expect("small row") - display_offset;

    {
        let sel = term.text_selection_mut();
        sel.start_selection(sel_row, 3, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(sel_row, 6, SelectionSide::Right);
        sel.complete_selection();
    }
    assert_paint_and_copy_agree(&mut term, 3, 20, wide_display_row, "scrollback wide edge");
    assert_eq!(term.selection_to_string().as_deref(), Some("漢字C"));
}
