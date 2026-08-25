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
///
/// The span reaches past the line NUMBER on purpose. `has_selection()` alone cannot
/// tell "the highlight stayed put" from "the highlight slid onto a neighbouring
/// line", and every row here starts with the same five characters — so the tests
/// below compare [`selected_text`], which is what a ⌘-C would actually return.
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
    sel.update_selection(-30, 9, SelectionSide::Right);
    sel.complete_selection();
    assert!(term.text_selection().has_selection());
    term
}

/// The text the live selection resolves to — the clipboard oracle. Panics when
/// there is no selection, so a caller that means "still there, still the same text"
/// cannot silently degrade into "gone".
fn selected_text(term: &Terminal) -> String {
    term.selection_to_string()
        .expect("a live selection must resolve to text")
}

/// THE REPORTED BUG. A program repainting a status bar in a scroll region at the
/// bottom of the screen must not destroy a highlight sitting in scrollback.
#[test]
fn a_region_scroll_spares_a_selection_it_never_touched() {
    let mut term = with_scrollback_selection(24, 40);
    let before = selected_text(&term);

    // DECSTBM rows 19..24, then scroll inside it — a status-bar repaint.
    term.process(b"\x1b[19;24r");
    term.process(b"\x1b[24;1H\n");

    assert!(
        term.text_selection().has_selection(),
        "a scroll region at the bottom must not clear a scrollback selection"
    );
    assert_eq!(
        selected_text(&term),
        before,
        "…and it must still name the SAME line: surviving over shifted content is \
         the wrong-copy half of the same bug"
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

    // DISCRIMINATOR. The assertion above passed under the pre-Phase-4 `i32::MAX`
    // sentinel too — that sentinel cleared EVERYTHING. Re-run the identical op with
    // the selection moved OUT of the region: the lattice must now spare it. The two
    // halves together fail under a blanket clear AND under a never-clear.
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

/// A reverse region scroll (RI inside DECSTBM) obeys the same rule.
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

/// Ordinary output at the live bottom, with the user reading history: the selection
/// rides the content and survives. This worked before (uniform scroll took the
/// `adjust_for_scroll` path, not the sentinel) — it is the standing control that the
/// lattice did not break the case that was already right.
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
    // The half `has_selection()` cannot see. Five uniform scrolls move every
    // anchor five rows; `adjust_for_scroll` is what keeps the highlight over the
    // same characters, and a control that never checks the TEXT would pass with the
    // compensation deleted — the selection would simply name a different line.
    assert_eq!(
        selected_text(&term),
        before,
        "the selection must ride the content, not sit still while it scrolls"
    );
}

/// The whole-coordinate-space cases keep clearing: ED 3 discards scrollback outright,
/// so no band can describe the damage and `All` is the honest answer.
#[test]
fn clearing_the_scrollback_still_clears_the_selection() {
    let mut term = with_scrollback_selection(24, 40);
    let before = selected_text(&term);

    // DISCRIMINATOR first: ED 2 erases the VISIBLE screen only. It records a band
    // over the live rows, which a scrollback anchor is disjoint from — so the
    // highlight survives, naming the same history line. Under the old `i32::MAX`
    // sentinel this cleared too, which is what made the ED 3 assertion below
    // pass for the wrong reason.
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

    let before = selected_text(&term);

    // Erase row 3, well away from the selection on row 0.
    term.process(b"\x1b[4;1H\x1b[K");

    assert!(
        term.text_selection().has_selection(),
        "EL on an unrelated row must not clear the selection"
    );
    assert_eq!(selected_text(&term), before, "…and must not move it");

    // DISCRIMINATOR. Sparing alone is what the PRE-Phase-4 code did for every EL
    // (it recorded nothing at all), so the assertion above cannot fail if the EL
    // band is reverted. Erasing the SELECTED row with the same op must clear —
    // which pins that the sparing is a scoped answer about overlap, not silence.
    term.process(b"\x1b[1;1H\x1b[K");
    assert!(
        !term.text_selection().has_selection(),
        "the same EL, aimed at the selected row, must clear the stale highlight"
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

    let before = selected_text(&term);

    // DECCARA: turn on bold over rows 1..2, cols 1..10.
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

    // DISCRIMINATOR. DECCARA is unmarked under every implementation of the lattice,
    // so the assertions above hold vacuously on their own. A DECCRA whose
    // DESTINATION is those same cells REPLACES the characters — same rows, same
    // columns, different question — and must clear. What separates the two is
    // whether the text changed, and this pins exactly that.
    term.process(b"\x1b[2;1;3;10;1;1;1$v");
    assert!(
        !term.text_selection().has_selection(),
        "replacing the characters under the highlight is not an attribute change"
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
    let before = selected_text(&term);
    // Copy rows 1..2 (over the selection) to row 15 — the source is only read.
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

    // NOT guarded on `has_selection()`. This test exists to pin the wrong-copy path
    // 613fab63 fixed, and a guard makes it PASS under the very regression it names:
    // restore the unconditional rows-resize clear and there is no selection left, so
    // a guarded body never runs and the test reports green while the behaviour it is
    // called after ("moves the selection with its content") is gone.
    assert!(
        term.text_selection().has_selection(),
        "a rows-only GROW must not destroy the selection — Phase 3 replaced the \
         unconditional clear precisely so it survives"
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

// ---------------------------------------------------------------------------
// The REST of the inverse hole: ECH / ICH / DCH, and ordinary character output.
//
// EL was marked on the argument that a `\r` + EL spinner left a highlight over
// replaced text. ECH, ICH and DCH are the same class — explicit CSI content
// replacement on the cursor's row — and recorded nothing, so the two ops differed
// only in which one got a mark: `\x1b[40X` left the stale highlight that `\x1b[K`
// cleared. Ordinary output is the third shape, and the one the spinner example
// actually names when the program does not bother with EL.
// ---------------------------------------------------------------------------

/// A 24x40 terminal with `hello world` on live row 0 and a completed selection over
/// `hello`. The cursor is left at the home position, so a test's first escape decides
/// which row the op lands on.
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

/// ECH (CSI X) blanks cells in place under the highlight. A background job repainting
/// a field with `\x1b[40X` is the shape: the GUI's clear-on-typing never fires for it,
/// so nothing else takes the highlight down and a copy returns 40 spaces.
#[test]
fn erasing_chars_on_the_selected_line_clears_the_stale_highlight() {
    let mut term = with_live_row_zero_selection();

    term.process(b"\x1b[1;1H\x1b[40X");

    assert!(
        !term.text_selection().has_selection(),
        "ECH rewrote the selected row; the highlight must not survive it"
    );
}

/// DCH (CSI P) shifts the row's tail LEFT under the highlight, so the copy comes back
/// shifted by N columns — text the user never selected, not even a prefix of it.
#[test]
fn deleting_chars_on_the_selected_line_clears_the_stale_highlight() {
    let mut term = with_live_row_zero_selection();

    term.process(b"\x1b[1;1H\x1b[3P");

    assert!(
        !term.text_selection().has_selection(),
        "DCH shifted the selected row's content; the highlight must not survive it"
    );
}

/// ICH (CSI @) shifts it RIGHT, the mirror image.
#[test]
fn inserting_chars_on_the_selected_line_clears_the_stale_highlight() {
    let mut term = with_live_row_zero_selection();

    term.process(b"\x1b[1;1H\x1b[3@");

    assert!(
        !term.text_selection().has_selection(),
        "ICH shifted the selected row's content; the highlight must not survive it"
    );
}

/// …and all three on ANOTHER row must spare it. Without this half the fix above is
/// indistinguishable from a return to clearing on every content op.
#[test]
fn char_edits_on_another_line_spare_the_highlight() {
    for op in [&b"\x1b[40X"[..], &b"\x1b[3P"[..], &b"\x1b[3@"[..]] {
        let mut term = with_live_row_zero_selection();

        // Row 3 (1-based row 4), well away from the selection on row 0.
        term.process(b"\x1b[4;1H");
        term.process(op);

        assert!(
            term.text_selection().has_selection(),
            "a char edit on an unrelated row must not clear the selection: {op:?}"
        );
    }
}

/// The canonical progress bar WITHOUT an erase: `\r` plus a plain overwrite. The
/// design used exactly this example, and only the `\r\e[K` spelling was closed —
/// ordinary character output recorded nothing, so the highlight stayed painted over
/// `Progress: 90%` and a copy returned it.
#[test]
fn a_bare_carriage_return_overwrite_clears_the_stale_highlight() {
    let mut term = Terminal::new(24, 40);
    term.process(b"Progress: 10%");
    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 12, SelectionSide::Right);
        sel.complete_selection();
    }
    assert!(term.text_selection().has_selection());

    term.process(b"\rProgress: 90%");

    assert!(
        !term.text_selection().has_selection(),
        "the row under the highlight was overwritten; the highlight must go with it"
    );
}

/// Every PRINT PATH records, not just the one the progress bar happens to take. The
/// parser splits printable text four ways — a bulk ASCII blast, a per-character
/// `print`, a bulk Unicode run, and REP's bulk arm, which reaches the grid without
/// going through `write_char` at all — and a bracket missing from any one of them is
/// a stale highlight for whichever program's output takes that shape.
#[test]
fn every_print_path_records_output_damage_on_the_row_it_lands() {
    // (tail of the SETUP batch, then the overwrite batch)
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

/// …and an overwrite on a row the selection does not touch must spare it — the
/// property that makes ordinary-output damage a BAND rather than the old
/// kill-everything sentinel. A background job repainting further down the screen is
/// the common case.
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

    // A genuine OVERWRITE of row 9, not an append onto blank cells: this must record
    // a band, and the band must miss row 0.
    term.process(b"\x1b[10;1Hbackground job OUTPUT");

    assert!(
        term.text_selection().has_selection(),
        "an overwrite on an unrelated row must not clear the selection"
    );
}

/// A print that APPENDS past a row's existing content REPLACES nothing, so it must
/// not clear a selection over that row. Not a nicety: a top-anchored history splice
/// (Codex `insert_history`) fills the blank row it just created, and the piecewise
/// remap deliberately carries a selection across that — marking the fill would
/// silently destroy the survival `EligibleSelectionUsesPiecewiseRemap` proves, and
/// would make ordinary appended output clear a selection on the same row.
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

    // Lands at column 5, one past `hello` — no cell the user selected changes.
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

/// TWO DISJOINT REPAINTS IN ONE BATCH. An inline TUI redrawing its title row and its
/// composer box in one PTY read produces two separated EL bands. They used to
/// hull-union into one band covering everything between them, destroying a selection
/// on rows nothing had rewritten — and adding EL to the marking set is what made that
/// reachable. The bands are kept apart now, up to `MAX_SELECTION_DAMAGE_BANDS`.
#[test]
fn two_disjoint_repaints_in_one_batch_spare_the_selection_between_them() {
    let mut term = Terminal::new(24, 40);
    for i in 0..24 {
        term.process(format!("row-{i}\r\n").as_bytes());
    }
    {
        let sel = term.text_selection_mut();
        sel.start_selection(10, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(10, 4, SelectionSide::Right);
        sel.complete_selection();
    }
    assert!(term.text_selection().has_selection());

    // ONE batch: the title row and the composer box, twenty rows apart.
    term.process(b"\x1b[1;1H\x1b[K\x1b[21;1H\x1b[K");

    assert!(
        term.text_selection().has_selection(),
        "nothing rewrote row 10; the gap between two disjoint bands is not damage"
    );
}

/// The bound is FOUR, and it is exact in both directions. Four disjoint repaints stay
/// four bands…
#[test]
fn four_disjoint_repaints_in_one_batch_spare_the_selection_between_them() {
    let mut term = Terminal::new(24, 40);
    {
        let sel = term.text_selection_mut();
        sel.start_selection(2, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(2, 4, SelectionSide::Right);
        sel.complete_selection();
    }

    term.process(b"\x1b[1;1H\x1b[K\x1b[5;1H\x1b[K\x1b[9;1H\x1b[K\x1b[13;1H\x1b[K");

    assert!(
        term.text_selection().has_selection(),
        "four disjoint bands are kept exactly; row 2 lies in a gap"
    );
}

/// …and so is a FIFTH, which the first version of this bound could not manage.
///
/// The accumulator originally held four bands and collapsed to the HULL on the
/// fifth, so a box border drawn in pieces and then filled — four regions plus one
/// drawn as two — lost every gap and cleared a selection nothing had rewritten. The
/// set now holds eight and, past that, absorbs the closest PAIR rather than
/// collapsing. This is the case that regressed.
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

/// Past the bound the set gives up precision, and it must give it up in the SAFE
/// direction: an over-clear, never a stale highlight over replaced text.
///
/// Nine separated repaints for a bound of eight. Whatever pair absorbs, the cover
/// still includes every row that was erased — so a selection ON one of them goes.
#[test]
fn past_the_bound_a_selection_on_a_repainted_row_still_clears() {
    let mut term = Terminal::new(24, 40);
    {
        let sel = term.text_selection_mut();
        // Row 4 is itself erased by the third repaint below — never in a gap.
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

/// The output-dedup guard must DIE with the batch that owns it.
///
/// `damage_selection_output` skips recording when a print lands on the row it just
/// recorded, so a run of prints into one row costs one band instead of thousands.
/// That guard is an assertion about the CURRENT accumulator, so `take_selection_damage`
/// clears it on drain. If it leaked across batches, the first print of the next batch
/// landing on the same row would record nothing — and a selection made in between
/// would survive text that replaced it, which is the stale-highlight, wrong-copy
/// failure this whole design exists to prevent.
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

    // Batch 1 overwrites the selected row, which clears the highlight and leaves the
    // guard pointing at that row.
    select_row_5(&mut term);
    term.process(b"\x1b[6;1Hfirst overwrite");
    assert!(
        !term.text_selection().has_selection(),
        "precondition: an overwriting print clears a selection on the row it replaced"
    );

    // Batch 2 lands on the SAME row. A leaked guard would suppress this mark.
    select_row_5(&mut term);
    assert!(term.text_selection().has_selection(), "precondition: re-selected");
    term.process(b"\x1b[6;1Hsecond overwrite");
    assert!(
        !term.text_selection().has_selection(),
        "the guard belongs to the drained batch; a second batch overwriting the same \
         row must record its own damage"
    );
}
