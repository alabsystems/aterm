// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The reading position and selection survive an alt-screen app (`less`, `vim`, `fzf`). The
//! batch prologue pins `display_offset` and forces it to 0 for processing, and
//! `enter_alternate_screen_raw` parks the main grid in `alt_grid`, so the epilogue re-pins the
//! grid that was pinned, not the fresh alt grid. The tests pin the observable property: the
//! same content is in view after the round trip.

use aterm_core::terminal::{TIERED_RING_CAP_DEFAULT, Terminal, TerminalBuilder};

/// Seed enough output to build scrollback, then scroll up into it.
fn scrolled_back(rows: u16, cols: u16, lines: usize) -> Terminal {
    let mut term = Terminal::new(rows, cols);
    for i in 0..lines {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    term.scroll_to_top();
    assert_ne!(
        term.grid().display_offset(),
        0,
        "the test must actually be scrolled back"
    );
    term
}

/// The top visible row's text. `Grid::cell` is the ring accessor, not the viewport reader
/// (`visible_row_view`): sound for ring-only grids with an empty lazy buffer, as the shallow
/// fixtures are, and in the tiered cases only at `display_offset == 0`.
fn top_row_text(term: &Terminal) -> String {
    let grid = term.grid();
    (0..grid.cols())
        .filter_map(|c| grid.cell(0, c).map(|cell| cell.char()))
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Entering the alt screen while scrolled back must not move the main viewport,
/// and leaving must put the user back on the same line.
#[test]
fn an_alt_screen_app_does_not_steal_the_reading_position() {
    let mut term = scrolled_back(6, 24, 64);
    let offset_before = term.grid().display_offset();
    let top_before = top_row_text(&term);

    // `less`/`vim`/`fzf`: enter the alt screen, paint, leave.
    term.process(b"\x1b[?1049h");
    term.process(b"a pager's screen\r\n");
    term.process(b"\x1b[?1049l");

    assert_eq!(
        term.grid().display_offset(),
        offset_before,
        "the main viewport must come back where it was"
    );
    assert_eq!(
        top_row_text(&term),
        top_before,
        "…and the SAME line must be under the eye"
    );
}

/// The same for the raw 47 / 1047 spellings, which take a different entry path.
#[test]
fn the_raw_alt_screen_modes_preserve_it_too() {
    for (enter, leave) in [
        (&b"\x1b[?47h"[..], &b"\x1b[?47l"[..]),
        (&b"\x1b[?1047h"[..], &b"\x1b[?1047l"[..]),
    ] {
        let mut term = scrolled_back(6, 24, 64);
        let offset_before = term.grid().display_offset();
        let top_before = top_row_text(&term);

        term.process(enter);
        term.process(b"scribble\r\n");
        term.process(leave);

        assert_eq!(
            term.grid().display_offset(),
            offset_before,
            "mode {enter:?}: the reading position must survive"
        );
        assert_eq!(top_row_text(&term), top_before, "mode {enter:?}: same line");
    }
}

/// Control: a reader at the live bottom stays there across the round trip.
#[test]
fn a_live_viewport_stays_live_across_the_round_trip() {
    let mut term = Terminal::new(6, 24);
    for i in 0..64 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    assert_eq!(term.grid().display_offset(), 0, "starts live");

    term.process(b"\x1b[?1049h");
    term.process(b"pager\r\n");
    term.process(b"\x1b[?1049l");

    assert_eq!(
        term.grid().display_offset(),
        0,
        "a live viewport must stay live"
    );
}

/// A rows-only resize keeps the same line in view. `display_offset` counts from the live
/// bottom, so `reflow.rs` re-anchors on `top_visible_absolute_row()` when the width is
/// unchanged, and only when `prev_offset > 0`: at offset 0 the anchor would scroll a live
/// viewport into history on a height shrink.
#[test]
fn a_rows_only_resize_keeps_the_same_line_under_the_eye() {
    for (from_rows, to_rows) in [(6u16, 4u16), (4, 6), (10, 3)] {
        let mut term = scrolled_back(from_rows, 24, 64);
        // Off the very top, so a clamp cannot produce the right answer by accident.
        term.scroll_display(-5);
        let top_before = top_row_text(&term);

        term.resize(to_rows, 24);

        assert_eq!(
            top_row_text(&term),
            top_before,
            "{from_rows}->{to_rows} rows: the top visible line must not move"
        );
    }
}

/// A terminal whose history genuinely offloads: the engine-default tiered store
/// behind a [`TIERED_RING_CAP_DEFAULT`]-line hot ring, so everything older than
/// that ring is compressed out of `Grid::cell`'s reach.
fn tiered_terminal(rows: u16, cols: u16, lines: usize) -> Terminal {
    let mut term = TerminalBuilder::new()
        .size(rows, cols)
        .tiered_scrollback_defaults()
        .build();
    for i in 0..lines {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    term
}

/// The same property on tiered history, parked deeper than the hot ring (where `Grid::cell`
/// reads `None`); the content oracle is `get_line_text`. The fixture is asserted to tier
/// first: a tiered test that never tiers is vacuous.
#[test]
fn a_rows_only_resize_keeps_the_anchor_through_offloaded_tiered_history() {
    for (from_rows, to_rows) in [(24u16, 12u16), (12, 24), (40, 5)] {
        let mut term = tiered_terminal(from_rows, 80, 20_000);

        // Prove the fixture first: `tiered_scrollback_lines()` counts store lines plus the
        // lazy buffer, all out of `Grid::cell`'s reach.
        let tiered = term.grid().tiered_scrollback_lines();
        assert!(
            tiered > 0,
            "{from_rows}->{to_rows}: fixture never reached tiered storage \
             (scrollback_lines={}, tiered={tiered}) — the test would be vacuous",
            term.grid().scrollback_lines()
        );
        let ring_lines = term.grid().scrollback_lines() - tiered;
        assert!(
            ring_lines <= TIERED_RING_CAP_DEFAULT,
            "{from_rows}->{to_rows}: the hot ring should hold at most \
             {TIERED_RING_CAP_DEFAULT} lines, holds {ring_lines}"
        );

        term.scroll_to_top();
        term.scroll_display(-200);
        let offset_before = i32::try_from(term.grid().display_offset()).unwrap();
        assert!(
            usize::try_from(offset_before).unwrap() > ring_lines,
            "{from_rows}->{to_rows}: the eye must be PAST the {ring_lines}-line hot \
             ring, i.e. on offloaded history, not merely scrolled back"
        );
        assert!(
            term.grid().cell(0, 0).is_none(),
            "{from_rows}->{to_rows}: the anchored row must be OFFLOADED — if the RING \
             accessor can still see it, this fixture is not testing the tiers"
        );

        let anchor_before = term.grid().top_visible_absolute_row();
        let text_before = term.get_line_text(-offset_before, None);
        assert!(
            text_before
                .as_deref()
                .is_some_and(|t| t.starts_with("line-")),
            "{from_rows}->{to_rows}: the resolving reader must find the tiered line"
        );

        term.resize(to_rows, 80);

        assert_eq!(
            term.grid().top_visible_absolute_row(),
            anchor_before,
            "{from_rows}->{to_rows} rows: the anchored absolute row must not move"
        );
        let offset_after = i32::try_from(term.grid().display_offset()).unwrap();
        assert!(
            offset_after > 0,
            "{from_rows}->{to_rows} rows: the reader must still be in history"
        );
        assert_eq!(
            term.get_line_text(-offset_after, None),
            text_before,
            "{from_rows}->{to_rows} rows: …and the same TIERED line must be under the eye"
        );
    }
}

/// The `prev_offset > 0` gate on the tiered fixture: a reader at the live bottom of a deep
/// tiered session stays live across a rows-only resize.
#[test]
fn a_live_tiered_viewport_stays_live_across_a_rows_only_resize() {
    for (from_rows, to_rows) in [(24u16, 12u16), (12, 24), (40, 5), (5, 40)] {
        let mut term = tiered_terminal(from_rows, 80, 20_000);
        assert!(
            term.grid().tiered_scrollback_lines() > 0,
            "{from_rows}->{to_rows}: fixture never reached tiered storage"
        );
        assert_eq!(term.grid().display_offset(), 0, "starts live");

        term.resize(to_rows, 80);

        assert_eq!(
            term.grid().display_offset(),
            0,
            "{from_rows}->{to_rows} rows: a tail follower must stay at the live bottom \
             even with 19 000 lines of tiered history behind it"
        );
        // `top_row_text` follows the offset (ring accessor at the viewport) while
        // `get_line_text(0)` is the live screen's top row read scroll-invariantly;
        // they agree only while the eye is on the live screen.
        assert_eq!(
            top_row_text(&term),
            term.get_line_text(0, None)
                .expect("the live top row is readable")
                .trim_end(),
            "{from_rows}->{to_rows} rows: the eye must be on the LIVE top row"
        );
    }
}

/// A resize while an alt-screen app is up (a window drag during `vim`):
/// `Terminal::resize` reflows the parked primary, which carries the reader's
/// offset, so the anchor runs on a grid that is not `self.grid`. Exit lands on
/// the line the user left.
#[test]
fn a_rows_only_resize_while_alt_is_up_still_re_anchors_the_parked_primary() {
    for (from_rows, to_rows) in [(6u16, 4u16), (4, 6)] {
        let mut term = scrolled_back(from_rows, 24, 64);
        term.scroll_display(-5);
        let top_before = top_row_text(&term);

        term.process(b"\x1b[?1049h");
        term.process(b"a pager's screen\r\n");
        term.resize(to_rows, 24);
        term.process(b"\x1b[?1049l");

        assert_eq!(
            top_row_text(&term),
            top_before,
            "{from_rows}->{to_rows} rows under alt: the same line must come back"
        );
    }
}

/// Control: a width change rewraps and renumbers rows, so no exact anchor
/// exists, but a scrolled-back reader still stays in history.
#[test]
fn a_width_resize_still_leaves_the_reader_in_history() {
    let mut term = scrolled_back(6, 24, 64);
    term.scroll_display(-5);
    assert_ne!(term.grid().display_offset(), 0);

    term.resize(6, 40);

    assert_ne!(
        term.grid().display_offset(),
        0,
        "a width change must not snap the reader to live"
    );
}

/// Control: a width change renumbers rows, so the selection clears rather than sit over the
/// wrong text.
#[test]
fn a_width_resize_still_clears_the_selection() {
    use aterm_core::selection::{SelectionSide, SelectionType};

    let mut term = Terminal::new(6, 24);
    for i in 0..64 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 4, SelectionSide::Right);
        sel.complete_selection();
    }

    term.resize(6, 40);

    assert!(
        !term.text_selection().has_selection(),
        "a width change rewraps, so the selection must still be cleared"
    );
}

// The selection is screen-scoped too: `post_process` parks the outgoing screen's selection
// and restores it on the way back, so a pager does not destroy a selection made on main.

use aterm_core::selection::{SelectionSide, SelectionType};

/// A terminal with a completed selection anchored in scrollback.
fn with_scrollback_selection(rows: u16, cols: u16) -> Terminal {
    let mut term = Terminal::new(rows, cols);
    for i in 0..64 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    let sel = term.text_selection_mut();
    sel.start_selection(-30, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(-30, 6, SelectionSide::Right);
    sel.complete_selection();
    assert!(term.text_selection().has_selection());
    assert!(term.selection_to_string().is_some());
    term
}

/// Every spelling of the switch, entry and exit paired.
const ALT_MODES: [(&[u8], &[u8]); 3] = [
    (b"\x1b[?1049h", b"\x1b[?1049l"),
    (b"\x1b[?47h", b"\x1b[?47l"),
    (b"\x1b[?1047h", b"\x1b[?1047l"),
];

/// Highlight something, run a pager, quit: the highlight is still over the same
/// text. Entry and exit in separate batches, as a pager's output arrives.
#[test]
fn a_selection_survives_an_alt_screen_app_in_every_mode() {
    for (enter, leave) in ALT_MODES {
        let mut term = with_scrollback_selection(6, 24);
        let start_before = term.text_selection().start();
        let end_before = term.text_selection().end();
        let text_before = term.selection_to_string();

        term.process(enter);
        term.process(b"a pager's screen\r\n");
        term.process(leave);

        assert!(
            term.text_selection().has_selection(),
            "mode {enter:?}: an alt-screen app must not destroy the main selection"
        );
        assert_eq!(term.text_selection().start().row, start_before.row);
        assert_eq!(term.text_selection().start().col, start_before.col);
        assert_eq!(term.text_selection().end().row, end_before.row);
        assert_eq!(term.text_selection().end().col, end_before.col);
        assert_eq!(
            term.selection_to_string(),
            text_before,
            "mode {enter:?}: the same TEXT must still be selected"
        );
    }
}

/// The whole round trip inside one `process` batch: `was_alt` is captured once
/// per batch, so an enter+exit that nets to no change skips both park and
/// restore.
#[test]
fn a_selection_survives_an_alt_round_trip_inside_one_batch() {
    for (enter, leave) in ALT_MODES {
        let mut term = with_scrollback_selection(6, 24);
        let text_before = term.selection_to_string();

        let mut batch = Vec::new();
        batch.extend_from_slice(enter);
        batch.extend_from_slice(b"a pager's screen\r\n");
        batch.extend_from_slice(leave);
        term.process(&batch);

        assert!(
            term.text_selection().has_selection(),
            "mode {enter:?}: a same-batch round trip must not destroy it either"
        );
        assert_eq!(term.selection_to_string(), text_before);
    }
}

/// Negative control: a selection made on the alt screen does not leak back to
/// main (it names the pager's buffer, which is gone); the restore overwrites it.
#[test]
fn a_selection_made_on_alt_does_not_leak_back_to_main() {
    for (enter, leave) in ALT_MODES {
        let mut term = Terminal::new(6, 24);
        for i in 0..64 {
            term.process(format!("line-{i}\r\n").as_bytes());
        }
        term.process(enter);
        term.process(b"pager row\r\n");
        {
            let sel = term.text_selection_mut();
            sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            sel.update_selection(0, 4, SelectionSide::Right);
            sel.complete_selection();
        }
        assert!(term.text_selection().has_selection(), "mode {enter:?}");

        term.process(leave);

        assert!(
            !term.text_selection().has_selection(),
            "mode {enter:?}: an alt-screen selection names a buffer the user can no longer see"
        );
    }
}

/// The main screen's selection stays parked however much the app scrolls its
/// own buffer: an alt full-screen scroll's uniform `content_scroll_delta` does
/// not touch the parked anchors.
#[test]
fn alt_screen_scrolling_does_not_walk_the_parked_selection() {
    let mut term = with_scrollback_selection(6, 24);
    let text_before = term.selection_to_string();

    term.process(b"\x1b[?1049h");
    for i in 0..40 {
        term.process(format!("pager row {i}\r\n").as_bytes());
    }
    term.process(b"\x1b[?1049l");

    assert_eq!(
        term.selection_to_string(),
        text_before,
        "40 rows of alt-screen scroll must not move a main-screen anchor"
    );
}

/// Exit direction of the cross-grid splice assert: a top-anchored DECSTBM archival scroll on
/// main leaves a `pending_selection_row_update` on the main grid when the same batch swaps it
/// out, and the swap-back batch drains it while `absolute_rows_before` names the alt grid's
/// counter; the debug assert exempts that batch. In every profile the splice archives one
/// more row above the anchor, so the anchor moves by exactly one row and keeps its text.
#[test]
fn a_stranded_splice_does_not_trip_the_cross_grid_assert_on_exit() {
    let mut term = with_scrollback_selection(6, 24);
    let text_before = term
        .selection_to_string()
        .expect("the fixture's selection resolves");
    let row_before = term.text_selection().start().row;

    // A protected-footer archival scroll (rows 1..=3 into history, rows below
    // fixed), in the same batch as the smcup that strands the splice.
    term.process(b"\x1b[r\x1b[1;1HX\x1b[1;3r\x1b[3;1H\n\x1b[?1049h");
    term.process(b"pager\r\n");
    term.process(b"\x1b[?1049l");

    // Reaching here in a debug build covers the assert; one row entered history above the
    // anchor.
    assert_eq!(
        term.text_selection().start().row,
        row_before - 1,
        "the stranded splice must be DRAINED on the batch that swaps the grid back"
    );
    // Draining against the alt grid's counter would move it off its own text.
    assert_eq!(
        term.selection_to_string().as_deref(),
        Some(text_before.as_str()),
        "the highlight must ride its content across the splice, not slide onto the \
         neighbouring line"
    );
    assert!(term.grid().scrollback_lines() > 0);
}

/// The enter direction: `enter_alternate_screen_raw` reuses the persistent alt
/// buffer, so whatever it recorded before being swapped out is drained on
/// re-entry while `absolute_rows_before` names the main grid's counter. The alt
/// buffer can never record a splice (no scrollback, no tiered store), only a
/// damage band and a scroll delta; draining them leaves the parked main
/// selection alone.
#[test]
fn stranded_alt_buffer_bookkeeping_survives_re_entry() {
    let mut term = with_scrollback_selection(6, 24);
    let text_before = term.selection_to_string();

    term.process(b"\x1b[?47h");
    // A region scroll plus an erase on alt, then leave with both stranded.
    term.process(b"\x1b[r\x1b[1;1HX\x1b[1;3r\x1b[3;1H\n\x1b[2J\x1b[?47l");
    term.process(b"\x1b[?47h");
    term.process(b"\x1b[?47l");

    assert_eq!(
        term.selection_to_string(),
        text_before,
        "the alt buffer's stale damage belongs to the alt buffer"
    );
}

/// A parked grid can lose scrollback while parked (lazy drain, retention
/// eviction) with no scroll delta or damage; the post-restore re-floor keeps
/// the anchor from pointing below the floor.
#[test]
fn scrollback_evicted_while_parked_does_not_resurrect_a_dangling_anchor() {
    let mut term = with_scrollback_selection(6, 24);
    let deepest = term
        .text_selection()
        .start()
        .row
        .min(term.text_selection().end().row);

    term.process(b"\x1b[?1049h");
    // Retention pressure against the parked main grid: the limit applies to the
    // grid that owns the primary content, which while alt is up is the saved one.
    term.set_scrollback_line_limit(Some(4));
    term.process(b"\x1b[?1049l");

    let floor = i32::try_from(term.grid().scrollback_lines()).unwrap_or(i32::MAX);
    assert!(
        floor < -deepest,
        "the fixture must actually evict the anchor: floor {floor}, anchor {deepest}"
    );
    assert!(
        !term.text_selection().has_selection()
            || term.text_selection().start().row >= -floor
                && term.text_selection().end().row >= -floor,
        "an anchor evicted while parked must never come back below the floor"
    );
}

/// A width resize while the pager is up rewraps the parked main grid, renumbering the rows
/// the parked selection is anchored to, so the selection goes.
#[test]
fn a_width_resize_while_on_alt_clears_the_parked_selection() {
    let mut term = with_scrollback_selection(6, 24);

    term.process(b"\x1b[?1049h");
    term.resize(6, 20);
    term.process(b"\x1b[?1049l");

    assert!(
        !term.text_selection().has_selection(),
        "a rewrap of the parked grid invalidates its anchors"
    );
}

/// A rows-only resize while on alt rewraps nothing, so the parked selection stays.
#[test]
fn a_rows_only_resize_while_on_alt_keeps_the_parked_selection() {
    let mut term = with_scrollback_selection(6, 24);
    let text_before = term.selection_to_string();

    term.process(b"\x1b[?1049h");
    term.resize(8, 24);
    term.process(b"\x1b[?1049l");

    assert!(
        term.text_selection().has_selection(),
        "a rows-only resize rewraps nothing, on either grid"
    );
    assert_eq!(
        term.selection_to_string(),
        text_before,
        "and the parked anchors must follow the parked grid's own revealed rows"
    );
}

/// The wholesale destroyers reach the parked slot too. Each of these runs while the
/// pager is up, and none of them goes through `post_process` on the way back.
#[test]
fn wholesale_destruction_while_on_alt_takes_the_parked_selection_with_it() {
    // ED 3 / `clear_scrollback`: erases both grids' history.
    let mut term = with_scrollback_selection(6, 24);
    term.process(b"\x1b[?1049h");
    term.clear_scrollback();
    term.process(b"\x1b[?1049l");
    assert!(
        !term.text_selection().has_selection(),
        "clear_scrollback erased the rows the parked anchors named"
    );

    // Direct `Terminal::reset` — an implicit alt->main swap outside the handler.
    let mut term = with_scrollback_selection(6, 24);
    term.process(b"\x1b[?1049h");
    term.reset();
    assert!(!term.is_alternate_screen(), "RIS leaves the alt screen");
    assert!(
        !term.text_selection().has_selection(),
        "a reset must not leave a parked selection standing"
    );

    // Byte-stream RIS — the same swap, reached from inside parser dispatch.
    let mut term = with_scrollback_selection(6, 24);
    term.process(b"\x1b[?1049h");
    term.process(b"\x1bc");
    assert!(!term.is_alternate_screen());
    assert!(
        !term.text_selection().has_selection(),
        "byte-stream RIS is a sixth implicit switch and must clear it too"
    );

    // Checkpoint hydration replaces the coordinate lineage under both slots. The
    // donor is captured on alt so the exit that follows is real and would hand
    // back a selection from a lineage this terminal never had.
    let mut donor = Terminal::new(6, 24);
    // Deep enough history that the post-restore re-floor cannot be what saves this:
    // a stale anchor at row -30 is comfortably inside the donor's own floor.
    for i in 0..64 {
        donor.process(format!("other-session-{i}\r\n").as_bytes());
    }
    donor.process(b"\x1b[?1049h");
    let checkpoint = donor.checkpoint();
    let mut term = with_scrollback_selection(6, 24);
    term.process(b"\x1b[?1049h");
    term.restore_checkpoint(&checkpoint);
    assert!(term.is_alternate_screen(), "the donor was captured on alt");
    term.process(b"\x1b[?1049l");
    assert!(
        !term.text_selection().has_selection(),
        "a hydrated session must not resurrect the pre-hydration selection on alt exit"
    );
}

/// RIS then re-entry into alt in one batch leaves no highlight: `was_alt` and
/// `modes.alternate_screen` are both true at `post_process`, so park and restore sit out the
/// batch, and the RIS clear and RIS's `SelectionDamage::All` on the main grid each dispose
/// of the parked selection. The clear alone is pinned by
/// `processing::tests::ris_empties_the_parked_selection_slot_in_the_batch_that_resets`.
#[test]
fn ris_then_reenter_alt_in_one_batch_leaves_no_surviving_highlight() {
    use aterm_core::selection::{SelectionSide, SelectionType};

    let mut term = Terminal::new(6, 24);
    for i in 0..40 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    // A selection on the main screen's live rows (in range before and after the
    // reset, so only the RIS clear disposes of it), parked by entering alt.
    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 4, SelectionSide::Right);
        sel.complete_selection();
    }
    term.process(b"\x1b[?1049h");
    assert!(
        !term.text_selection().has_selection(),
        "entering alt parks the main selection"
    );

    // RIS and re-enter alt in one batch: neither park nor restore fires.
    term.process(b"\x1bc\x1b[?1049h");
    // Leave alt: the slot is empty, nothing to hand back.
    term.process(b"\x1b[?1049l");

    assert!(
        !term.text_selection().has_selection(),
        "RIS must destroy the parked selection; it cannot survive to be restored \
         over a grid the reset erased"
    );
}

// Resize, mid-batch exit and re-entry edge cases.

/// Every visible row's text, top to bottom. Same ring-accessor caveat as
/// [`top_row_text`].
fn window_text(term: &Terminal) -> Vec<String> {
    let grid = term.grid();
    (0..grid.rows())
        .map(|r| {
            (0..grid.cols())
                .filter_map(|c| grid.cell(r, c).map(|cell| cell.char()))
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// A completed selection over `[row, 0]..=[row, cols]`, with its text returned.
fn select_row(term: &mut Terminal, row: i32, cols: u16) -> String {
    {
        let sel = term.text_selection_mut();
        sel.start_selection(row, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(row, cols, SelectionSide::Right);
        sel.complete_selection();
    }
    term.selection_to_string()
        .expect("the fixture must select real text")
}

/// A rows-only shrink keeps a scrollback selection over its text: `adjust_row_count_rows_only`
/// demotes the top `old - new` viewport rows into history, so every anchor moves back that
/// many lines.
#[test]
fn a_rows_only_shrink_keeps_a_scrollback_selection_over_its_own_text() {
    let mut term = Terminal::new(10, 24);
    for i in 0..64 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    let text_before = select_row(&mut term, -20, 6);
    // Pin the literal text, so this cannot pass by comparing two wrong answers.
    assert_eq!(text_before, "line-35");

    // A window-height drag / font zoom / find-bar toggle: four rows shorter,
    // same width.
    term.resize(6, 24);

    assert_eq!(
        term.selection_to_string().as_deref(),
        Some(text_before.as_str()),
        "the shrink pushed four viewport rows on top of history; the anchor owes \
         them exactly one compensation each"
    );
}

/// A selection on a live row past the new height keeps its text: the demote relabels live
/// rows and history by the same count.
#[test]
fn a_rows_only_shrink_follows_a_demoted_live_row_into_history() {
    let mut term = Terminal::new(10, 24);
    for i in 0..64 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    // Row 7 of 10 lies past the new height; the demote of rows 0..=3 moves it to row 3.
    let text_before = select_row(&mut term, 7, 6);
    assert_eq!(text_before, "line-62");

    term.resize(6, 24);

    assert_eq!(
        term.selection_to_string().as_deref(),
        Some(text_before.as_str()),
        "a demoted row keeps its content; the highlight must follow it into history"
    );
}

/// The shrink map applies to the parked selection too: `finalize_resize` runs
/// it against the saved primary, which a height drag under `vim` resizes.
#[test]
fn a_rows_only_shrink_under_alt_keeps_the_parked_scrollback_selection() {
    let mut term = Terminal::new(10, 24);
    for i in 0..64 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    let text_before = select_row(&mut term, -20, 6);

    term.process(b"\x1b[?1049h");
    term.resize(6, 24);
    term.process(b"\x1b[?1049l");

    assert_eq!(
        term.selection_to_string().as_deref(),
        Some(text_before.as_str()),
        "the parked grid took the same bottom-push and owes the same compensation"
    );
}

/// An rmcup mid-batch must not leave the restored grid scrolled back. The
/// parked main grid carries `display_offset > 0`, and the rest of the batch
/// writes through `row_index`, which subtracts it: without flattening, `\x1b[K`
/// and `HELLO` would land in history instead of on the live screen.
#[test]
fn an_rmcup_mid_batch_writes_to_the_live_screen_not_into_history() {
    let mut term = scrolled_back(6, 24, 64);
    let window_before = window_text(&term);
    assert!(
        window_before.iter().any(|row| row.starts_with("line-")),
        "the fixture must be parked over real history"
    );

    term.process(b"\x1b[?1049h");
    term.process(b"a pager's screen\r\n");
    // The rmcup and the bytes after it, in one read.
    term.process(b"\x1b[?1049l\r\x1b[KHELLO\n");

    assert_eq!(
        window_text(&term),
        window_before,
        "not one row of the user's history may be erased or overwritten by bytes \
         that belong on the live screen"
    );

    term.scroll_to_bottom();
    assert!(
        window_text(&term).iter().any(|row| row == "HELLO"),
        "…and the write must have landed on the LIVE screen: {:?}",
        window_text(&term)
    );
}

/// Output before the smcup in the same batch counts toward the re-pin: those
/// lines entered the main grid's scrollback before it was parked.
#[test]
fn output_before_an_smcup_in_the_same_batch_still_moves_the_pin() {
    let mut term = scrolled_back(6, 24, 64);
    let top_before = top_row_text(&term);

    // Three lines of job output, then the app's smcup — one read.
    term.process(b"a\r\nb\r\nc\r\n\x1b[?1049h");
    term.process(b"pager\r\n");
    term.process(b"\x1b[?1049l");

    assert_eq!(
        top_row_text(&term),
        top_before,
        "the three lines that entered the main grid's scrollback before the smcup \
         must be part of the restored reading position"
    );
}

/// A batch that exits and re-enters alt runs neither park arm
/// (`\x1b[?1049l\x1b[?47h` looks like "still on alt"). The exit dropped the alt
/// buffer and `?47h` allocated a blank one without damage, so the old alt
/// selection must be cleared via `alt_screen_left_in_batch`.
#[test]
fn an_exit_and_re_entry_in_one_batch_kills_the_previous_alt_selection() {
    let mut term = Terminal::new(6, 24);
    for i in 0..40 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    // A main-screen selection, parked by the entry — it must survive all of this.
    let main_text = select_row(&mut term, -10, 6);
    term.process(b"\x1b[?1049h");
    // Home first: 1049 keeps the cursor where main left it (the bottom row), and
    // this selection has to name text the alt buffer really holds.
    term.process(b"\x1b[Hpager row\r\n");
    // …and a selection made on the alt screen, which must not survive.
    select_row(&mut term, 0, 4);

    term.process(b"\x1b[?1049l\x1b[?47h");

    assert!(
        !term.text_selection().has_selection(),
        "the 1049 exit destroyed that alt buffer; its highlight cannot outlive it"
    );

    term.process(b"\x1b[?47l");
    assert_eq!(
        term.selection_to_string().as_deref(),
        Some(main_text.as_str()),
        "and the parked MAIN selection is still the one that comes back"
    );
}

/// A shrink absorbed by trailing blanks moves no anchor: the relabel distance is the demote
/// count, not the height delta (blank rows below the cursor are dropped, not archived). The
/// two are equal on a full screen, so this uses a partly empty one.
#[test]
fn a_shrink_absorbed_entirely_by_trailing_blanks_leaves_the_anchor_alone() {
    let mut term = Terminal::new(10, 24);
    // Four lines of content; the cursor lands on row 4 and rows 4..9 stay blank.
    for i in 0..4 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    let text_before = select_row(&mut term, 1, 6);
    assert_eq!(text_before, "line-1");

    // 10 -> 6 is a delta of four, and there are six blank rows below the cursor, so
    // the trim absorbs the whole shrink: demote == 0.
    term.resize(6, 24);

    assert_eq!(
        term.text_selection().start().row,
        1,
        "nothing was archived, so the anchor keeps its row"
    );
    assert_eq!(
        term.selection_to_string().as_deref(),
        Some(text_before.as_str()),
        "and it still names the same text"
    );
}
