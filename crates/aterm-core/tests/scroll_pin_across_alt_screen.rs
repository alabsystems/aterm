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

/// The top visible row's text — what the user is looking at.
///
/// `Grid::cell` is the RING accessor, not the viewport reader (`visible_row_view`
/// is), so this oracle is only sound for a ring-only grid with an empty lazy
/// buffer — which every fixture here is, at 24 columns and a few dozen lines. On a
/// deep-history grid a scrolled-back row can be tiered and legitimately read back
/// as blank; do not lift this helper into such a test without swapping the reader.
///
/// The two tiered cases below DO use it — but only at `display_offset == 0`, where
/// the top visible row is the live screen's own and is always ring-resident by
/// construction. That is the one place the warning does not bite.
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

/// SELECTION CUSTODY — a ROWS-ONLY resize keeps the same line under the eye.
///
/// `display_offset` is measured from the live bottom, so restoring the same offset
/// under a different `visible_rows` slides the content by exactly the row-count
/// delta. Window height changes, font zoom and horizontal divider drags are all
/// rows-only resizes, so this fired on every one of them — and Phase 1's removal of
/// the font-zoom snap made it visible rather than hidden.
///
/// `reflow.rs` now re-anchors on `top_visible_absolute_row()` when the width is
/// unchanged, because a rows-only resize rewraps nothing and leaves
/// `absolute_row_counter` alone: the row under the eye keeps its absolute number.
///
/// This WAS attempted once and reverted, on a misread that is worth recording so it
/// is not re-derived. The revert was triggered by
/// `fuzz_process_never_panics::reflow_wide_char_resize_never_panics` failing with
/// "cell (0,0) inaccessible on a 1x82 grid", and the conclusion drawn was that
/// `display_offset <= scrollback_lines()` is a broken bound. It is not. `Grid::cell`
/// is the RING accessor (`GridStorage::row_index`) and legitimately returns `None`
/// for a viewport parked on lazy or tiered history — `grid/tests/style_perf.rs`
/// `row_returns_none_when_scrolled_into_tiered_scrollback` pins exactly that, and
/// ⌘↑ (`scroll_to_top`) drives production to that same bound every day. The viewport
/// reader is `visible_row_view`, not `Grid::cell`. What actually went wrong is that
/// the reverted patch re-anchored even at `prev_offset == 0`, where the arithmetic
/// wants `d + (v - t) > 0` on a height shrink: it scrolled LIVE viewports into
/// history, which is the only reason that fuzz ever saw a nonzero offset. The
/// `prev_offset > 0` gate is the fix; the fuzz oracle now asserts its own
/// offset-0 precondition so the same false signal cannot recur.
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

/// The same property on DEEP history at the retention cap, read through the
/// resolving reader rather than the ring accessor.
///
/// `top_row_text` above is `Grid::cell`, the RING accessor, which legitimately reads
/// blank once the anchored row has been lazily buffered or tiered — that miscue is
/// what got the first attempt reverted. This case therefore asserts on the absolute
/// row itself and reads the line through `get_line_text`, which resolves ring, lazy
/// and tiered alike. It also exercises the retention cap: the anchor arm is allowed
/// to clamp there, so the assertion is on the anchor and the offset is checked only
/// for still being in history.
///
/// HONEST SCOPE — this fixture does NOT reach TIERED storage, despite what an earlier
/// name for it claimed. Measured: 20 000 lines through `Terminal::new(rows, 80)`
/// gives `scrollback_lines = 10000`, `tiered_scrollback_lines = 0` — the ring
/// retention cap bites first, so nothing is ever offloaded. The real guard against
/// the reverted failure mode is
/// `fuzz_process_never_panics::reflow_wide_char_resize_never_panics`, which drives
/// 30 000 randomised resize/reflow rounds and is the gate the first attempt failed.
/// A genuine tiered case needs a `TerminalBuilder` with a tiered store configured;
/// it now exists, immediately below —
/// `a_rows_only_resize_keeps_the_anchor_through_offloaded_tiered_history`.
#[test]
fn a_rows_only_resize_keeps_the_anchor_through_deep_capped_history() {
    for (from_rows, to_rows) in [(24u16, 12u16), (12, 24), (40, 5)] {
        let mut term = Terminal::new(from_rows, 80);
        for i in 0..20_000 {
            term.process(format!("line-{i}\r\n").as_bytes());
        }
        term.scroll_to_top();
        // Off the very top, so a clamp cannot accidentally produce the right answer.
        term.scroll_display(-200);
        let anchor_before = term.grid().top_visible_absolute_row();
        let offset_before = i32::try_from(term.grid().display_offset()).unwrap();
        let text_before = term.get_line_text(-offset_before, None);
        assert!(
            text_before
                .as_deref()
                .is_some_and(|t| t.starts_with("line-")),
            "{from_rows}->{to_rows}: the fixture must be reading real history"
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
            "{from_rows}->{to_rows} rows: …and the same line must be under the eye"
        );
    }
}

/// A terminal whose history genuinely OFFLOADS: the engine-default tiered store
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

/// The same property on genuinely TIERED history — the case the fixture above
/// cannot reach, and the exact shape the reverted first attempt was blamed for.
///
/// `Terminal::new` keeps raw cells in the ring and the retention cap bites before
/// anything is offloaded, so that fixture proves nothing about the compressed
/// store. `tiered_scrollback_defaults()` puts a [`TIERED_RING_CAP_DEFAULT`]-line
/// hot ring in front of the tiered store, so 20 000 lines leave ~19 000 of them
/// OFF the ring. Measured here: `scrollback_lines ≈ 19 977`, of which
/// `tiered_scrollback_lines ≈ 18 977` — and this test asserts that BEFORE it
/// asserts anything else, because a tiered test that never tiers is vacuous.
///
/// The row parked under the eye is then deliberately deeper than the hot ring, so
/// `Grid::cell(0, 0)` reads `None` — the literal "cell (0,0) inaccessible"
/// condition that got the anchor reverted. That is not a bug: `Grid::cell` is the
/// RING accessor, and a viewport parked on offloaded history has no ring row for
/// it to return. The assertion on it here is a PRECONDITION, pinning that this
/// fixture really is in the offloaded region; the content oracle is
/// `get_line_text`, which resolves ring, lazy and tiered alike.
#[test]
fn a_rows_only_resize_keeps_the_anchor_through_offloaded_tiered_history() {
    for (from_rows, to_rows) in [(24u16, 12u16), (12, 24), (40, 5)] {
        let mut term = tiered_terminal(from_rows, 80, 20_000);

        // PROVE THE FIXTURE FIRST: history must really have left the ring.
        // NOTE `tiered_scrollback_lines()` returns `store.line_count() +
        // lazy_buffer.len()`, so this proves the history left the HOT RING — not
        // that every one of those lines is compressed. That is the property the
        // anchor cares about: `Grid::cell` serves the ring, and anything off it is
        // out of its reach whichever tier holds it.
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
        // Off the very top, so a clamp cannot accidentally produce the right answer.
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

/// The `prev_offset > 0` gate, on the tiered fixture: a reader at the LIVE bottom
/// of a deep tiered session must stay live across a rows-only resize.
///
/// This is the half of the gate no SCROLLED-BACK case can see — above offset 0 the
/// gate is a no-op, so widening it to `prev_offset >= 0` is invisible up there.
/// `a_live_viewport_stays_live_across_a_rows_only_resize` above already covers that
/// half on a ring-only grid, and both fail against the widened gate; what THIS case
/// adds is the same property with 19 000 lines of offloaded history behind it, where
/// the offset the widened gate demands would land the reader in the compressed
/// store rather than merely a few rows up.
/// At offset 0 it re-anchors the live top row, and on a height SHRINK the anchor
/// arithmetic `d + (v - t)` then demands a positive offset: every window-height
/// drag would push a tail-following reader into (here, compressed) history. That
/// is what the resize fuzzers saw, and this is the test that catches it.
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
        // Non-redundant content check: `top_row_text` FOLLOWS the offset (it is the
        // ring accessor at the viewport), while `get_line_text(0)` is the live
        // screen's own top row, read scroll-invariantly. They agree only while the
        // eye is genuinely on the live screen; a re-anchor into history moves the
        // first and not the second — and on this fixture it moves it onto a
        // compressed row the ring cannot read at all, so it reads back blank.
        assert_eq!(
            top_row_text(&term),
            term.get_line_text(0, None)
                .expect("the live top row is readable")
                .trim_end(),
            "{from_rows}->{to_rows} rows: the eye must be on the LIVE top row"
        );
    }
}

/// Control for the `prev_offset > 0` gate, and the reason the first attempt at the
/// anchor was reverted: a reader at the LIVE bottom must stay there.
///
/// The anchor arithmetic is `want = d + (v - t)`. At `d == 0` a height SHRINK still
/// wants a positive offset, so re-anchoring unconditionally scrolls a tail-following
/// viewport into history on every window-height drag — and that, not any bound in
/// `scroll_to_absolute_row`, is what made the resize fuzzers see a nonzero offset.
#[test]
fn a_live_viewport_stays_live_across_a_rows_only_resize() {
    for (from_rows, to_rows) in [(6u16, 4u16), (4, 6), (10, 3), (3, 10)] {
        let mut term = Terminal::new(from_rows, 24);
        for i in 0..64 {
            term.process(format!("line-{i}\r\n").as_bytes());
        }
        assert_eq!(term.grid().display_offset(), 0, "starts live");

        term.resize(to_rows, 24);

        assert_eq!(
            term.grid().display_offset(),
            0,
            "{from_rows}->{to_rows} rows: a tail follower must stay at the live bottom"
        );
    }
}

/// The resize can arrive while an alt-screen app is up — a window drag during `vim`
/// is the everyday case. `Terminal::resize` then reflows the PARKED primary
/// (`callback_setters.rs`: `saved_primary.resize(..)`), which still carries the
/// reader's nonzero offset, so the anchor arm runs on a grid that is not `self.grid`.
/// Exit must land on the same line the user left.
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

// ===========================================================================
// SELECTION CUSTODY — the SELECTION is screen-scoped too.
//
// The viewport half above proved the reading position survives a pager. The
// highlight did not: every alt-screen switch recorded `SelectionDamage::All` on
// the incoming grid, so `vim`/`less`/`fzf` destroyed a selection made on the main
// screen — content the alt buffer never touched and could not have invalidated.
//
// The fix parks the outgoing screen's selection at the top of `post_process` and
// restores it on the way back. These tests pin the OBSERVABLE property: the same
// TEXT is still selected afterwards, not merely that some selection exists.
// ===========================================================================

use aterm_core::selection::{SelectionSide, SelectionType};

/// A terminal with deep history holding a completed selection anchored in
/// SCROLLBACK — the case an alt-screen app has no business touching.
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

/// THE SELECTION HALF OF THE REPORTED BUG. Highlight something, run a pager, quit:
/// the highlight must still be over the same text.
///
/// Entry and exit in SEPARATE batches, which is the real shape — a pager's output
/// arrives over many reads, and the enter batch is the one that used to destroy it.
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

/// The whole round trip inside ONE `process` batch. `was_alt` is captured once per
/// batch, so an enter+exit pair that nets to no change must also net to no change
/// for the selection — the park and the restore both have to be skipped, not run
/// once each.
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

/// Control: a selection on the LIVE screen survives the round trip too. It is the
/// main grid's content, and the main grid comes back untouched.
#[test]
fn a_live_screen_selection_survives_the_round_trip() {
    let mut term = Terminal::new(6, 24);
    for i in 0..64 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    {
        let sel = term.text_selection_mut();
        sel.start_selection(1, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(1, 6, SelectionSide::Right);
        sel.complete_selection();
    }
    let text_before = term.selection_to_string();

    term.process(b"\x1b[?1049h");
    term.process(b"pager\r\n");
    term.process(b"\x1b[?1049l");

    assert_eq!(
        term.selection_to_string(),
        text_before,
        "a live-screen selection names main-grid content, which came back intact"
    );
}

/// NEGATIVE CONTROL — the park is ASYMMETRIC on purpose. A selection made ON the
/// alt screen must not leak back to main: it names the pager's buffer, which is
/// gone. Restoring the main selection is a `mem::take`, so it overwrites.
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

/// The main screen's selection stays parked for as long as the app runs, however
/// much the app scrolls its own buffer. An alt full-screen scroll advances the alt
/// grid's `absolute_row_counter` and takes the uniform `content_scroll_delta` path —
/// which, before the park, was applied to the MAIN screen's anchors.
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

/// THE SPLICE ASSERT, EXIT DIRECTION. A top-anchored DECSTBM archival scroll on
/// main leaves a `pending_selection_row_update` stranded on the main grid when the
/// next batch swaps it out; the batch that swaps it back drains it while
/// `absolute_rows_before` still names the ALT grid's counter. The debug assert
/// compares those two, and before the exemption it panicked in every debug build.
///
/// The debug assert alone is not an oracle: under `cargo test --release` it is
/// compiled out, and `scrollback_lines() > 0` — which this test used to assert on
/// its own — is already true of the fixture before the round trip starts, so in that
/// profile the test checked nothing at all. What the drain is FOR is pinned instead,
/// against an ABSOLUTE oracle rather than a same-code control: the splice archives
/// one more row above the anchor, so the anchor must move by exactly one row and
/// keep naming the same characters. A stranded splice that is dropped, or applied
/// against the wrong grid's counter, breaks one of those two — in either profile.
#[test]
fn a_stranded_splice_does_not_trip_the_cross_grid_assert_on_exit() {
    let mut term = with_scrollback_selection(6, 24);
    let text_before = term
        .selection_to_string()
        .expect("the fixture's selection resolves");
    let row_before = term.text_selection().start().row;

    // A protected-footer archival scroll: rows 1..=3 move into history while the
    // rows below stay fixed (the shape of `processing.rs`'s own splice fixture), in
    // the SAME batch as the smcup that strands the resulting splice.
    term.process(b"\x1b[r\x1b[1;1HX\x1b[1;3r\x1b[3;1H\n\x1b[?1049h");
    term.process(b"pager\r\n");
    term.process(b"\x1b[?1049l");

    // Reaching here in a DEBUG build is the assert half. These two run in both
    // profiles. LIVENESS first — one row entered history above the anchor, so the
    // drain must have moved it; an un-drained splice leaves the row untouched.
    assert_eq!(
        term.text_selection().start().row,
        row_before - 1,
        "the stranded splice must be DRAINED on the batch that swaps the grid back"
    );
    // …and CORRECTNESS: moving the anchor is only right if it still names its own
    // content. Draining against the alt grid's counter moves it off.
    assert_eq!(
        term.selection_to_string().as_deref(),
        Some(text_before.as_str()),
        "the highlight must ride its content across the splice, not slide onto the \
         neighbouring line"
    );
    assert!(term.grid().scrollback_lines() > 0);
}

/// THE ENTER DIRECTION. `enter_alternate_screen_raw` REUSES the persistent alt
/// buffer, so whatever that buffer recorded on the batch that swapped it out is
/// still on it — nothing drains a parked grid — and the re-entry batch drains it
/// while `absolute_rows_before` names the MAIN grid's counter.
///
/// The guard on the splice assert is symmetric for that reason, but be precise
/// about what is reachable: a SPLICE cannot be recorded on the alt buffer at all.
/// `Grid::scroll_region_up` only takes the archival top-anchored path when
/// `max_scrollback > 0 || scrollback.is_some() || scrollback_detached_for_reflow`
/// (`grid/scroll.rs`), and the alt buffer is always `Grid::with_scrollback(rows,
/// cols, 0)` with no tiered store and no offload — `set_scrollback_line_limit` and
/// `resize_offloading_scrollback` both target the PRIMARY-content grid, which while
/// alt is up is the saved one. So the enter direction can strand a damage band and a
/// scroll delta, never a splice. What this pins is that draining them on re-entry
/// leaves the parked main selection alone.
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

/// A parked grid can lose scrollback WHILE PARKED — the compression worker drains
/// its lazy buffer, retention evicts. That happens with no scroll delta and no
/// damage, so only the post-restore re-floor can catch it. Without it the anchor
/// comes back pointing below the floor, at rows that no longer exist.
#[test]
fn scrollback_evicted_while_parked_does_not_resurrect_a_dangling_anchor() {
    let mut term = with_scrollback_selection(6, 24);
    let deepest = term
        .text_selection()
        .start()
        .row
        .min(term.text_selection().end().row);

    term.process(b"\x1b[?1049h");
    // Retention pressure against the PARKED main grid: the limit applies to the
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

/// A WIDTH resize while the pager is up rewraps the parked main grid, renumbering
/// exactly the rows the parked selection is anchored to. It must go.
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

/// …and its rows-only twin must NOT. Nothing rewrapped; a window-height drag or a
/// font zoom while `vim` is up has no business eating the highlight underneath.
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

/// The wholesale destroyers reach the PARKED slot too. Each of these runs while the
/// pager is up, and none of them goes through `post_process` on the way back.
#[test]
fn wholesale_destruction_while_on_alt_takes_the_parked_selection_with_it() {
    // ED 3 / `clear_scrollback`: erases BOTH grids' history.
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

    // Checkpoint hydration swaps the whole coordinate lineage underneath both slots.
    // The donor is captured ON alt so its `modes.alternate_screen` keeps the hydrated
    // terminal there: the exit that follows is a REAL exit, and it is the one that
    // would hand back a selection anchored in a lineage this terminal never had.
    let mut donor = Terminal::new(6, 24);
    // Deep enough history that the post-restore re-floor CANNOT be what saves this:
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

/// RIS followed by a re-entry into alt IN ONE BATCH — no highlight may survive it.
///
/// `\x1bc\x1b[?1049h` leaves `was_alt` and `modes.alternate_screen` BOTH true at
/// `post_process`, so the park/restore pair sits out the batch entirely: a stale
/// pre-RIS main selection stays in the parked slot and is a candidate to be handed
/// back on the next `?1049l`, over a grid the reset already erased.
///
/// SCOPE, corrected. The shipped note said two mechanisms enforce this and named
/// `truncate_to_floor`'s floor-0 guard as the second. Measured by mutation, that is
/// wrong in both directions: the anchor is on a LIVE row here (in range on any grid
/// of the same height, so no bounds check and no floor clamp can reach it), and
/// deleting BOTH explicit `parked_text_selection.clear()` calls — the
/// `pending_parser_reset` one and `Terminal::reset`'s — still leaves this GREEN.
///
/// The real second enforcer is RIS's own `erase_scrollback`, which records
/// `SelectionDamage::All` on the main grid. That band is not drained while the alt
/// grid is active, so the exit batch drains it in `post_process` right AFTER the
/// restore and destroys the handed-back selection on arrival. No black-box test can
/// separate the two, because the erase and the clear are the same event.
///
/// So this stays a PROPERTY test, honestly labelled, and the clear itself is
/// isolated one batch earlier and one layer down, on the field invariant it exists
/// to keep: `terminal::processing::tests::ris_empties_the_parked_selection_slot_in_the_batch_that_resets`.
#[test]
fn ris_then_reenter_alt_in_one_batch_leaves_no_surviving_highlight() {
    use aterm_core::selection::{SelectionSide, SelectionType};

    let mut term = Terminal::new(6, 24);
    for i in 0..40 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    // A selection on the MAIN screen's LIVE rows — in range before and after the
    // reset, so nothing but the RIS clear can dispose of it. Then park it by
    // entering alt.
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

    // RIS and re-enter alt in ONE batch: neither park nor restore fires.
    term.process(b"\x1bc\x1b[?1049h");
    // Now leave alt. The slot must be empty — nothing to hand back.
    term.process(b"\x1b[?1049l");

    assert!(
        !term.text_selection().has_selection(),
        "RIS must destroy the parked selection; it cannot survive to be restored \
         over a grid the reset erased"
    );
}

// ===========================================================================
// SELECTION CUSTODY Phase 3, the second audit — the state-machine holes the
// narrowing left behind.
// ===========================================================================

/// Every visible row's text, top to bottom — the whole window under the eye.
///
/// Same ring-accessor caveat as [`top_row_text`]: sound only for the shallow,
/// ring-only fixtures in this file.
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

/// A ROWS-ONLY SHRINK moves a scrollback selection with its content.
///
/// The mirror of the rows-GROW case above, and the one the narrowing missed.
/// `take_last_resize_row_shift()` is only ever set on a grow, so the shrink ran
/// `adjust_for_scroll(0, ..)` — anchors unchanged — on the premise that a rows-only
/// resize moves no content. It does: `adjust_row_count_rows_only` pushes the bottom
/// `old - new` viewport rows on top of history, so every SCROLLBACK anchor gains
/// that many lines above it. Nothing was cleared and nothing was flagged, so the
/// highlight silently sat on later text and a copy returned lines the user never
/// picked.
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

/// The other half of the same map: a selection on a row the shrink DEMOTES.
///
/// Those rows are not evicted and they are not still live — they become the newest
/// history, at `row - old_rows`. A uniform delta cannot describe that and the same
/// row's live neighbours above the cut at once, which is why this is its own
/// primitive rather than another `adjust_for_scroll` argument.
#[test]
fn a_rows_only_shrink_follows_a_demoted_live_row_into_history() {
    let mut term = Terminal::new(10, 24);
    for i in 0..64 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    // Row 7 of 10 is below the cut at 6 — it is demoted to relative -3.
    let text_before = select_row(&mut term, 7, 6);
    assert_eq!(text_before, "line-62");

    term.resize(6, 24);

    assert_eq!(
        term.selection_to_string().as_deref(),
        Some(text_before.as_str()),
        "a demoted row keeps its content; the highlight must follow it into history"
    );
}

/// The SHRINK direction for the PARKED selection too — `finalize_resize` runs the
/// identical map against the saved primary grid, which a height drag under `vim`
/// resizes just the same.
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

/// AN RMCUP MID-BATCH MUST NOT LEAVE THE RESTORED GRID SCROLLED BACK.
///
/// The Phase-3 re-pin parks the main grid carrying `display_offset > 0` — that is
/// how the reading position survives a pager. `exit_alternate_screen` then swaps
/// that grid back in MID-batch, and every remaining byte is written through
/// `row_index`, which SUBTRACTS `display_offset`: the `\x1b[K` erased a line of the
/// user's history 40 rows back and `HELLO` was written into that same row, so the
/// live screen never showed it. Two secondary consequences rode along — the damage
/// band named the LIVE row while the erase hit a scrollback row (a stale highlight
/// over blanked text), and a full-screen scroll in that window snapped the reader
/// to live with no pin left to restore it.
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
    // The rmcup AND the bytes that follow it, in ONE read — the shape every
    // isolated-`?1049l` test in this file misses.
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

/// OUTPUT BEFORE THE SMCUP IN THE SAME BATCH COUNTS TOWARD THE RE-PIN.
///
/// The parked-grid re-pin hardcoded `lines_added = 0` behind "a grid that has been
/// swapped out stopped receiving output". It stopped receiving it only AFTER the
/// swap: batch boundaries are `read()` boundaries, so job output followed by an
/// app's smcup in one read is routine, and those lines really did enter the MAIN
/// grid's scrollback.
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

/// A BATCH THAT EXITS AND RE-ENTERS ALT RUNS NEITHER PARK ARM.
///
/// `post_process` compares the batch's start screen with its end screen, so
/// `\x1b[?1049l\x1b[?47h` looks like "still on alt" and neither `mem::take` fires.
/// The 1049 exit DROPPED that alt buffer and `?47h` allocated a fresh blank one —
/// which, unlike `enter_alternate_screen`, records no damage — so the old alt
/// screen's anchors stayed live over a buffer that never held the selected text.
#[test]
fn an_exit_and_re_entry_in_one_batch_kills_the_previous_alt_selection() {
    let mut term = Terminal::new(6, 24);
    for i in 0..40 {
        term.process(format!("line-{i}\r\n").as_bytes());
    }
    // A main-screen selection, parked by the entry — it must SURVIVE all of this.
    let main_text = select_row(&mut term, -10, 6);
    term.process(b"\x1b[?1049h");
    // Home first: 1049 keeps the cursor where main left it (the bottom row), and
    // this selection has to name text the alt buffer really holds.
    term.process(b"\x1b[Hpager row\r\n");
    // …and a selection made ON the alt screen, which must not survive.
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

/// A shrink that only TRIMS moves nothing, so the selection must not move either.
///
/// The relabel distance is the DEMOTE count, not the height delta. Trailing blank
/// rows strictly below the cursor are not content: the shrink drops them outright
/// rather than archiving them, because archiving would manufacture blank history a
/// later grow reveals above real text. When the height delta is absorbed entirely by
/// that trim, no row changes position at all.
///
/// Compensating by the height delta instead would push every anchor up by the
/// trimmed count — a highlight over text the user never selected, which is the
/// wrong-copy direction. Cheap to get wrong because the two numbers are equal on a
/// FULL screen, which is what every other test in this file uses.
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
