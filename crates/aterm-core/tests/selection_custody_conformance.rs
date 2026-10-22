// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Selection custody: damage is a question about overlap.
//!
//! Grid operations record selection damage as absolute-row bands, and a
//! selection is cleared iff it overlaps one. So a status bar scrolling rows
//! 18-23 spares a highlight anchored in scrollback, and a `\r` + EL progress
//! bar rewriting the highlighted row clears it (a copy must never return text
//! the user did not select).

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;

/// A terminal with history, scrolled to the top, holding a completed selection anchored in
/// scrollback. The span reaches past the line number on purpose: every row starts with
/// `line-`, so only [`selected_text`] (what a copy returns) tells a highlight that stayed put
/// from one that slid onto a neighbouring line.
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

/// The same op clears a selection inside the region.
#[test]
fn a_region_scroll_clears_a_selection_inside_it() {
    let mut term = Terminal::new(24, 40);
    for i in 0..64 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    // Select on a live row inside the region we are about to scroll.
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

    // Discriminator: a blanket clear passes the assertion above. Re-run the same
    // op with the selection outside the region: it must be spared. Together the
    // halves fail under both a blanket clear and a never-clear.
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

/// Ordinary output at the live bottom, with the user reading history: the selection rides
/// the content and survives.
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
    // Five uniform scrolls move every anchor five rows (`adjust_for_scroll`); only the text
    // shows the highlight stayed over the same characters.
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

    // Discriminator first: ED 2 erases the visible screen only; its band over the
    // live rows is disjoint from a scrollback anchor, so the highlight survives
    // on the same history line. A blanket clear would fail here.
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

/// EL (the `\r` + EL progress bar or spinner) on a different row leaves the
/// highlight alone, and the same EL on the selected row clears it: a copy must
/// never return text the user did not select, and the rule is scoped to
/// overlap rather than a return to clearing everything.
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

    // Discriminator: sparing alone is also what recording nothing does. Erasing
    // the selected row with the same op must clear, so the sparing is a scoped
    // answer about overlap.
    term.process(b"\x1b[1;1H\x1b[K");
    assert!(
        !term.text_selection().has_selection(),
        "the same EL, aimed at the selected row, must clear the stale highlight"
    );
}

/// DECCARA changes attributes only — the characters, and therefore what a copy
/// returns, are unchanged. It must not clear the selection.
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

    // Discriminator: a DECCRA onto the same cells replaces the characters and must clear;
    // what separates the two is whether the text changed.
    term.process(b"\x1b[2;1;3;10;1;1;1$v");
    assert!(
        !term.text_selection().has_selection(),
        "replacing the characters under the highlight is not an attribute change"
    );
}

/// DECCRA copies a rectangle; the destination cells are replaced, so a highlight over
/// them is stale and must go — while the source, only read, is untouched.
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

/// A rows grow that reveals history moves the selection with its content:
/// `Grid::resize_with_reflow_mode` shifts every pre-resize viewport row down by `revealed`,
/// and the selection moves by the same amount as the cursors.
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

    // Rows-only grow, which reveals retained history above the old viewport.
    term.resize(10, 20);

    // Not guarded on `has_selection()`: an unconditional clear on rows-resize
    // would leave no selection, and a guarded body would then pass vacuously.
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

/// A terminal whose history is capped at `limit` and full, so the next line evicts; the
/// anchors are left to the caller.
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

/// Partial eviction: scrolling one endpoint of a selection past the history
/// floor clamps that endpoint onto the oldest retained row and reports the
/// loss, keeping the part still on screen, as the copy walk does
/// (`selection_to_string_capped` reads `first_row = adj_start_row.max(-history)`).
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

/// Control: with both endpoints evicted there is nothing to clamp onto, so the selection
/// clears, with no sticky `incomplete`.
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

/// Eviction with no delta: shrinking the retention limit drops the oldest lines without a
/// scroll or a damage band, so neither `adjust_for_scroll` nor the damage test in
/// `post_process` sees it.
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

/// The alt screen has no scrollback, so no oldest retained row to clamp onto: a scroll that
/// evicts an endpoint clears the selection rather than pinning it to alt row 0
/// (`min_row >= 0` in `clamp_head_to_min_row`).
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
    // Select on alt with the lower endpoint one scroll away from the floor and the
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

// ECH / ICH / DCH and ordinary character output: like EL, each replaces content on the
// cursor's row, so each records selection damage.

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

/// ECH, DCH and ICH on another row spare the highlight. Without this half the
/// clearing below is indistinguishable from clearing on every content op.
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

/// ECH, ICH and DCH on the highlighted row clear it: the control for the spare test above.
#[test]
fn char_edits_on_the_selected_line_clear_the_stale_highlight() {
    for op in [&b"\x1b[40X"[..], &b"\x1b[3P"[..], &b"\x1b[3@"[..]] {
        let mut term = with_live_row_zero_selection();

        // Cursor to row 0 (1-based 1;1), directly over the highlight.
        term.process(b"\x1b[1;1H");
        term.process(op);

        assert!(
            !term.text_selection().has_selection(),
            "a char edit ON the selected row must clear the stale highlight: {op:?}"
        );
    }
}

/// Every print path records: the parser splits printable text four ways (a
/// bulk ASCII blast, per-character `print`, a bulk Unicode run, and REP's bulk
/// arm, which bypasses `write_char`), and a missing bracket in any one is a
/// stale highlight for output of that shape.
#[test]
fn every_print_path_records_output_damage_on_the_row_it_lands() {
    // (tail of the setup batch, then the overwrite batch)
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

/// An overwrite on a row the selection does not touch spares it: ordinary-output damage is
/// a band, not a blanket clear.
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

    // A genuine overwrite of row 9, not an append onto blank cells: this must record
    // a band, and the band must miss row 0.
    term.process(b"\x1b[10;1Hbackground job OUTPUT");

    assert!(
        term.text_selection().has_selection(),
        "an overwrite on an unrelated row must not clear the selection"
    );
}

/// A print that appends past a row's content replaces nothing, so it spares a selection on
/// that row. A top-anchored history splice (Codex `insert_history`) fills the blank row it
/// just created, and the piecewise remap carries a selection across it
/// (`EligibleSelectionUsesPiecewiseRemap`); marking the fill would destroy that.
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

/// Five disjoint repaints stay five bands: the set holds eight and past that merges the
/// closest pair rather than collapsing to the hull, so a box border drawn in pieces keeps its
/// gaps.
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

/// Past the bound (nine separated repaints for eight bands) the set loses precision in the
/// safe direction: the merged cover still includes every erased row, so a selection on one
/// of them goes.
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

/// The output-dedup guard dies with its batch: `damage_selection_output` skips recording a
/// print on the row it just recorded, and `take_selection_damage` clears that guard on drain,
/// so the next batch's first print on that row records its own damage.
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

    // Batch 2 lands on the same row. A leaked guard would suppress this mark.
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
