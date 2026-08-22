// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SELECTION CUSTODY Phase 3 — the reading position survives an alt-screen app.
//!
//! The reported bug, in the user's words: "the scroll position is lost because some
//! other program is running". Its viewport half is this: scrolling back into history
//! and then running ANY alt-screen program — `less`, `man`, `vim`, `fzf`, `git log`
//! — destroyed the reading position permanently.
//!
//! Mechanism (`terminal/processing.rs`): the batch prologue pins `display_offset`
//! and forces it to 0, because VT row arithmetic requires that during processing.
//! `enter_alternate_screen_raw` then `mem::replace`s the main grid — carrying that
//! forced zero — into `alt_grid`. The epilogue's re-pin used to always target
//! `self.grid`, which by then is the fresh ALT grid, whose `scrollback_lines()` is 0,
//! so the re-pin clamped to 0 and did nothing. Nothing ever restored the main grid's
//! offset, and exit swaps the main grid back wholesale — zero included.
//!
//! The fix repins the grid that was actually pinned. These tests pin the OBSERVABLE
//! property: the same content is under the eye after the round-trip.

use aterm_core::terminal::Terminal;

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

/// The top visible row's text — what the user is looking at.
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

/// Entry and exit split across SEPARATE `process` batches — the real shape, since a
/// pager's output arrives over many reads. The batch that ENTERS alt is the one that
/// used to lose the offset.
#[test]
fn the_position_survives_when_entry_and_exit_are_separate_batches() {
    let mut term = scrolled_back(6, 24, 64);
    let offset_before = term.grid().display_offset();
    let top_before = top_row_text(&term);

    term.process(b"\x1b[?1049h");
    for i in 0..20 {
        term.process(format!("pager row {i}\r\n").as_bytes());
    }
    term.process(b"\x1b[?1049l");

    assert_eq!(term.grid().display_offset(), offset_before);
    assert_eq!(top_row_text(&term), top_before);
}

/// Control: while the user is at the LIVE bottom, an alt-screen app must still
/// leave them at the live bottom. The fix must not invent an offset.
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

/// SELECTION CUSTODY Phase 3 — KNOWN GAP, attempted and reverted. See below.
///
/// `display_offset` is measured from the live bottom, so restoring the same offset
/// under a different `visible_rows` slides the content by exactly the row-count
/// delta. Window height changes, font zoom and horizontal divider drags are all
/// rows-only resizes, so this fires on every one of them — and Phase 1's removal of
/// the font-zoom snap made it visible rather than hidden.
///
/// The obvious fix — capture `top_visible_absolute_row()` before the resize and
/// restore with `scroll_to_absolute_row()` after — WAS implemented and DOES fix this
/// case, but it fails `fuzz_process_never_panics::reflow_wide_char_resize_never_panics`
/// with "cell (0,0) inaccessible on a 1x82 grid". Isolated to `reflow.rs` alone by
/// stashing each file in turn; the other Phase 3 changes are clean.
///
/// Root cause as far as it was chased: on a rows-only SHRINK the anchor legitimately
/// demands a LARGER offset than before (`visible_rows` fell, so the same top line now
/// sits further from the live bottom), which clamps to `scrollback_lines()`. That
/// count includes LAZY and TIERED lines that are not ring-resident
/// (`state/scrollback.rs:142`), and at that boundary — mid-reflow, right after
/// `restore_reflowed_scrollback` splices — a visible cell stops resolving.
///
/// The real finding is therefore about an EXISTING invariant, not about this design:
/// `display_offset <= scrollback_lines()` (the postcondition `scroll_to_absolute_row`
/// and `repin_display_offset` both assert) is evidently WEAKER than "every visible
/// cell is addressable". Anything that drives the offset to that bound can expose it.
/// Reverted rather than guessed at: this is the resize path the L0 whole-Mac-freeze
/// work owns, and a wrong bound here is not a cosmetic bug.
#[ignore = "known gap: the anchor restore trips a latent addressability bound at max display_offset; see the doc comment"]
#[test]
fn a_rows_only_resize_keeps_the_same_line_under_the_eye() {
    for (from_rows, to_rows) in [(6u16, 4u16), (4, 6), (10, 3)] {
        let mut term = scrolled_back(from_rows, 24, 64);
        // Sit somewhere in the middle of history rather than at the very top,
        // so a clamp cannot accidentally produce the right answer.
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

/// Control: a WIDTH change rewraps and renumbers rows, so no exact anchor exists.
/// It must still leave a scrolled-back reader IN history rather than snapping them
/// to the live bottom.
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

/// SELECTION CUSTODY Phase 3 — a ROWS-ONLY resize keeps the SELECTION too.
///
/// `finalize_resize` used to clear unconditionally, on the reasoning that reflow
/// invalidates all row/col coordinates. That is true of a width change, which
/// rewraps and renumbers rows — but a rows-only resize rewraps nothing. Font zoom
/// and a window height drag were throwing selections away for a resize that could
/// not have invalidated them.
#[test]
fn a_rows_only_resize_keeps_the_selection() {
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
    assert!(term.text_selection().has_selection());

    term.resize(4, 24);

    assert!(
        term.text_selection().has_selection(),
        "a rows-only resize rewraps nothing and must keep the selection"
    );
}

/// Control: a WIDTH change genuinely renumbers rows, so it must still clear.
/// Losing the highlight is right there; keeping it would leave it over the wrong text.
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
