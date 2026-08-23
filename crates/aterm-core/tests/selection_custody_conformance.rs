// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SELECTION CUSTODY Phase 4 — damage is a question about OVERLAP.
//!
//! The selection half of the reported bug: "the scroll position is lost because some
//! other program is running and filling the bottom of the screen" — and with it, the
//! highlight.
//!
//! Before this phase, twenty-five grid sites set `content_scroll_delta = i32::MAX`, a
//! sentinel meaning "kill the selection". It was applied backwards in BOTH
//! directions:
//!
//! * a status bar scrolling rows 18-23 destroyed a highlight anchored at row -40 in
//!   scrollback — content it never touched;
//! * a `\r` + EL progress bar rewrote the row UNDER a live highlight and left it in
//!   place, so a copy returned text the user never selected.
//!
//! The lattice replaces the sentinel with an absolute-row BAND, and a selection is
//! cleared iff it actually overlaps.

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;

/// A terminal with history, scrolled back, holding a completed selection on the
/// oldest visible line — i.e. a selection anchored in SCROLLBACK.
fn with_scrollback_selection(rows: u16, cols: u16) -> Terminal {
    let mut term = Terminal::new(rows, cols);
    for i in 0..64 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    term.scroll_to_top();
    assert_ne!(term.grid().display_offset(), 0, "scrolled back");
    // Anchor well above the live screen: row -30 is deep in retained history.
    let sel = term.text_selection_mut();
    sel.start_selection(-30, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(-30, 4, SelectionSide::Right);
    sel.complete_selection();
    assert!(term.text_selection().has_selection());
    term
}

/// THE REPORTED BUG. A program repainting a status bar in a scroll region at the
/// bottom of the screen must not destroy a highlight sitting in scrollback.
#[test]
fn a_region_scroll_spares_a_selection_it_never_touched() {
    let mut term = with_scrollback_selection(24, 40);

    // DECSTBM rows 19..24, then scroll inside it — a status-bar repaint.
    term.process(b"\x1b[19;24r");
    term.process(b"\x1b[24;1H\n");

    assert!(
        term.text_selection().has_selection(),
        "a scroll region at the bottom must not clear a scrollback selection"
    );
}

/// …and the same op MUST still clear a selection that IS inside the region.
/// Over-clearing is safe; leaving a highlight over replaced content is not.
#[test]
fn a_region_scroll_clears_a_selection_inside_it() {
    let mut term = Terminal::new(24, 40);
    for i in 0..64 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    // Select on a LIVE row inside the region we are about to scroll.
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
}

/// A reverse region scroll (RI inside DECSTBM) obeys the same rule.
#[test]
fn a_reverse_region_scroll_spares_a_scrollback_selection() {
    let mut term = with_scrollback_selection(24, 40);

    term.process(b"\x1b[19;24r");
    term.process(b"\x1b[19;1H\x1bM");

    assert!(
        term.text_selection().has_selection(),
        "a reverse region scroll must not reach into scrollback"
    );
}

/// Ordinary output at the live bottom, with the user reading history: the selection
/// rides the content and survives. This worked before (uniform scroll took the
/// `adjust_for_scroll` path, not the sentinel) — it is the standing control that the
/// lattice did not break the case that was already right.
#[test]
fn ordinary_output_still_preserves_a_scrollback_selection() {
    let mut term = with_scrollback_selection(24, 40);

    for i in 0..5 {
        term.process(format!("more-{i}\r\n").as_bytes());
    }

    assert!(
        term.text_selection().has_selection(),
        "a uniform scroll must keep a scrollback selection"
    );
}

/// The whole-coordinate-space cases keep clearing: ED 3 discards scrollback outright,
/// so no band can describe the damage and `All` is the honest answer.
#[test]
fn clearing_the_scrollback_still_clears_the_selection() {
    let mut term = with_scrollback_selection(24, 40);

    term.process(b"\x1b[3J");

    assert!(
        !term.text_selection().has_selection(),
        "ED 3 destroys the coordinate space; the selection must go with it"
    );
}

/// The INVERSE hole: an op that REWRITES the row a highlight sits on must clear it.
/// EL recorded nothing before, so a `\r` + EL progress bar or spinner rewrote the row
/// under a live highlight and left it painted — a copy then returned text the user
/// never selected. Over-clearing is safe; a stale highlight is not.
#[test]
fn erasing_the_selected_line_clears_the_stale_highlight() {
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

    // The spinner shape: carriage return, erase the line, repaint.
    term.process(b"\r\x1b[K");

    assert!(
        !term.text_selection().has_selection(),
        "EL rewrote the selected row; the highlight must not survive it"
    );
}

/// …but EL on a DIFFERENT row must leave the highlight alone. This is the half that
/// makes the previous test a scoped rule rather than a return to clearing everything.
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

    // Erase row 3, well away from the selection on row 0.
    term.process(b"\x1b[4;1H\x1b[K");

    assert!(
        term.text_selection().has_selection(),
        "EL on an unrelated row must not clear the selection"
    );
}

/// DECCARA changes attributes only — the characters, and therefore what a copy
/// returns, are unchanged. It must NOT clear the selection.
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

    // DECCARA: turn on bold over rows 1..2, cols 1..10.
    term.process(b"\x1b[1;1;2;10;1$r");

    assert!(
        term.text_selection().has_selection(),
        "an attribute-only change rewrites no text; the highlight still names it"
    );
}

/// DECCRA copies a rectangle; the DESTINATION cells are replaced, so a highlight over
/// them is stale and must go — while the SOURCE, only read, is untouched.
#[test]
fn a_rect_copy_clears_over_its_destination_but_spares_its_source() {
    // Destination overlaps the selection.
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
    // DECCRA: copy rows 1..2 to row 6 (1-based) => dest rows 5..6 (0-based).
    term.process(b"\x1b[1;1;2;10;1;6;1$v");
    assert!(
        !term.text_selection().has_selection(),
        "the destination rows were overwritten; the highlight is stale"
    );

    // Source overlaps the selection, destination far away.
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
    // Copy rows 1..2 (over the selection) to row 15 — the source is only read.
    term.process(b"\x1b[1;1;2;10;1;15;1$v");
    assert!(
        term.text_selection().has_selection(),
        "reading the selected rows as a copy SOURCE must not clear the highlight"
    );
}

/// REGRESSION (introduced by Phase 3's `finalize_resize` narrowing, found by the
/// Phase 5 planning sweep): a rows-GROW that reveals history must move the selection
/// with its content.
///
/// `Grid::resize_with_reflow_mode` computes `revealed` and compensates the cursor and
/// the saved cursor for it, with a comment that says every pre-resize viewport row
/// "now sits `revealed` rows further down" (`reflow.rs:235-244`). Nothing compensated
/// the SELECTION. That was harmless while `finalize_resize` cleared unconditionally —
/// Phase 3 replaced the clear with `adjust_for_scroll(0, ..)`, a delta of ZERO, so the
/// anchors stay put while the text moves down: the highlight ends up over different
/// text and a copy returns something the user never selected.
#[test]
fn a_rows_grow_that_reveals_history_moves_the_selection_with_its_content() {
    let mut term = Terminal::new(4, 20);
    for i in 0..40 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    // Select a whole visible row and remember exactly what it says.
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

    // Rows-only GROW, which reveals retained history above the old viewport.
    term.resize(10, 20);

    if term.text_selection().has_selection() {
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
}

/// A terminal whose retained history is capped at `limit` and full, with the anchors
/// left to the caller. `limit` lines of history plus a live screen is the only shape
/// in which eviction is reachable without waiting for a default-sized scrollback to
/// fill.
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

/// PARTIAL EVICTION. Scrolling one endpoint of a selection past the history floor
/// used to destroy the whole highlight — including the part still on screen and still
/// copyable. It now clamps that endpoint onto the oldest retained row and REPORTS the
/// loss, which is what the copy walk has done for years: `selection_to_string_capped`
/// reads `first_row = adj_start_row.max(-history)`.
#[test]
fn evicting_one_endpoint_truncates_instead_of_destroying_the_selection() {
    let mut term = with_capped_history(24, 40, 50);
    {
        let sel = term.text_selection_mut();
        // -50 is the oldest retained row; the span reaches four rows newer.
        sel.start_selection(-50, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(-46, 6, SelectionSide::Right);
        sel.complete_selection();
    }
    let before = term
        .selection_to_string()
        .expect("the five-row span resolves to text");
    assert!(!term.text_selection().truncated(), "nothing lost yet");

    // Two more lines of output evict two more: rows -50 and -49 are gone.
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

/// The non-vacuity control: once BOTH endpoints are evicted there is nothing to clamp
/// onto and the honest answer is still a clear, with no sticky ` incomplete`.
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

/// EVICTION WITH NO DELTA. Shrinking the retention limit drops the oldest lines
/// without scrolling anything and without recording a damage band, so neither the
/// geometric transform nor the damage test in `post_process` sees it. This is the
/// entry point `adjust_for_scroll` cannot serve.
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

/// THE ALT-SCREEN GUARD, at the terminal level. The alt grid is always
/// `Grid::with_scrollback(rows, cols, 0)`, so its floor is row 0 and there is no
/// oldest RETAINED row to clamp onto. Clamping would pin the highlight to alt row 0 —
/// content the user never selected — and a full-screen alt scroll takes the uniform
/// delta path and records no damage band that could catch it afterwards. The guard is
/// structural (`min_row == 0`), not a test of `alternate_screen`, so it holds for a
/// primary grid configured with zero scrollback too.
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
    // Select on alt with the LOWER endpoint one scroll away from the floor and the
    // upper endpoint safely inside it — the shape a both-endpoints case cannot test.
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
