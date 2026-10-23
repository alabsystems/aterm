// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Verifies that stationary scrolled-back repaints hit the `HistoryEpoch` viewport memo
//! and that wheel notches, new output, and Kitty unscrolls materialize only invalidated rows.

use aterm_core::render::RenderInput;
use aterm_core::terminal::Terminal;
use aterm_grid::test_counters::take_viewport_row_materialize;
use aterm_scrollback::Scrollback;

const ROWS: u16 = 24;
const COLS: u16 = 80;
const FILL: usize = 2_000;
const DEPTH: i32 = 200;
const NOTCH: i32 = 3;

fn filled_terminal() -> Terminal {
    let mut term = Terminal::new(ROWS, COLS);
    let mut corpus = String::with_capacity(FILL * 24);
    for i in 0..FILL {
        corpus.push_str(&format!("sb {i:05} the quick brown fox jumps over it\r\n"));
    }
    term.process(corpus.as_bytes());
    term
}

fn filled_tiered_terminal() -> Terminal {
    let store = Scrollback::new(FILL, FILL * 2, 256 * 1024 * 1024);
    let mut term = Terminal::with_scrollback(ROWS, COLS, 8, store);
    let mut corpus = String::with_capacity(FILL * 24);
    for i in 0..FILL {
        corpus.push_str(&format!("sb {i:05} the quick brown fox jumps over it\r\n"));
    }
    term.process(corpus.as_bytes());
    term
}

fn frame(term: &mut Terminal, scratch: &mut RenderInput) {
    term.cell_frame_into(scratch, usize::from(ROWS), usize::from(COLS));
}

#[test]
fn a_stationary_scrolled_back_repaint_materializes_no_history_rows() {
    let mut term = filled_terminal();
    let mut scratch = RenderInput::empty();

    frame(&mut term, &mut scratch);
    let _ = take_viewport_row_materialize();
    for _ in 0..4 {
        frame(&mut term, &mut scratch);
    }
    assert_eq!(
        take_viewport_row_materialize(),
        0,
        "live-bottom control: a frame at the bottom reads no history at all"
    );

    term.scroll_display(DEPTH);
    frame(&mut term, &mut scratch);
    assert_eq!(
        take_viewport_row_materialize(),
        usize::from(ROWS) + 1,
        "the first scrolled-back frame must materialize the whole viewport (plus \
         the apron row) from the tiers — if it does not, this workload never \
         reached the SCR-1 path"
    );

    for _ in 0..16 {
        frame(&mut term, &mut scratch);
    }
    assert_eq!(
        take_viewport_row_materialize(),
        0,
        "16 stationary scrolled-back repaints re-materialized history rows — the \
         SCR-1 viewport memo is not memoizing. This is the 24-per-frame regime \
         the campaign removed, back again."
    );
}

#[test]
fn a_wheel_notch_pays_only_for_the_rows_that_scrolled_in() {
    let mut term = filled_terminal();
    let mut scratch = RenderInput::empty();
    term.scroll_display(DEPTH);
    frame(&mut term, &mut scratch);
    let _ = take_viewport_row_materialize();

    term.scroll_display(NOTCH);
    frame(&mut term, &mut scratch);
    let moved = take_viewport_row_materialize();
    assert_eq!(
        moved,
        usize::try_from(NOTCH).expect("NOTCH is positive"),
        "a {NOTCH}-line wheel notch materialized {moved} rows; it must pay for \
         the rows that scrolled IN and nothing else"
    );

    frame(&mut term, &mut scratch);
    assert_eq!(
        take_viewport_row_materialize(),
        0,
        "the repaint after a notch settles back to zero"
    );
}

#[test]
fn output_arriving_while_scrolled_back_reopens_the_bill() {
    let mut term = filled_terminal();
    let mut scratch = RenderInput::empty();
    term.scroll_display(DEPTH);
    frame(&mut term, &mut scratch);
    let _ = take_viewport_row_materialize();
    frame(&mut term, &mut scratch);
    assert_eq!(take_viewport_row_materialize(), 0, "parked and warm");

    term.process(b"a line arrives while you are reading\r\n");
    frame(&mut term, &mut scratch);
    assert!(
        take_viewport_row_materialize() > 0,
        "content changed under a scrolled-back viewport and the memo served the \
         old rows anyway — that is a stale glyph on screen, not a saving"
    );
}

#[test]
fn a_kitty_unscroll_renumbers_history_and_reopens_the_bill() {
    let mut term = filled_tiered_terminal();
    let mut scratch = RenderInput::empty();
    term.scroll_display(DEPTH);
    frame(&mut term, &mut scratch);
    let _ = take_viewport_row_materialize();
    frame(&mut term, &mut scratch);
    assert_eq!(take_viewport_row_materialize(), 0, "parked and warm");

    let lines_before = term.grid().scrollback_lines();
    let gen_before = term.grid().content_gen();
    let renumber_before = term.grid().history_renumber_epoch();
    let abs_before = term.grid().absolute_row_counter();
    term.process(b"\x1b[3+T");
    assert!(
        term.grid().scrollback_lines() < lines_before,
        "the CSI +T unscroll removed no scrollback line ({lines_before} -> {}) \
         — nothing was renumbered, so this fixture is not exercising the \
         renumber epoch at all. The unscroll pulls from the ring and the \
         store alike, so the fixture above must have left history to pull.",
        term.grid().scrollback_lines()
    );
    assert!(
        term.grid().history_renumber_epoch() > renumber_before,
        "the unscroll did not advance history_renumber_epoch — the signal this \
         fixture exists to pin never moved"
    );
    assert_eq!(
        term.grid().content_gen(),
        gen_before,
        "content_gen MOVED across the unscroll. That would make the renumber \
         epoch redundant here and this fixture would be pinning content_gen \
         over again — the one thing `output_arriving_…` already covers."
    );
    assert_eq!(
        term.grid().absolute_row_counter(),
        abs_before,
        "the absolute counter moved, so every memo key shifted on its own and \
         the misses below would prove nothing about the epoch"
    );
    assert_eq!(
        term.grid().display_offset(),
        usize::try_from(DEPTH).expect("DEPTH is positive"),
        "the viewport left its parking depth, so the next frame asks for a \
         different key set and would miss for reasons of its own"
    );

    let _ = take_viewport_row_materialize();
    frame(&mut term, &mut scratch);
    assert!(
        take_viewport_row_materialize() > 0,
        "history was renumbered wholesale under a scrolled-back viewport and the \
         memo kept serving the rows it had cached under the OLD absolute \
         numbers. That is a screen of wrong lines, not a saving — and \
         `content_gen` does not move here, so nothing else in this file sees it. \
         (In a debug build the memo's own stale-hit net fires first, from \
         `visible_row_view.rs`; this assertion is the release-build half.)"
    );
}
