// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! A stationary scrolled-back repaint materializes no history rows.
//!
//! A scrolled-back viewport keeps its rows in a memo keyed by
//! `HistoryEpoch`, so repeated frames (pill fade, cursor blink, effects,
//! selection-drag mouse moves) cost nothing and a wheel notch pays only for
//! the rows that scrolled in. Counts are exact and machine-independent, so
//! they guard the structure (a memo that stops memoizing, an O(N) walk, a key
//! that stops matching); they cannot see a constant-factor slowdown.
//!
//! Every zero is two-sided: the first scrolled-back frame must materialize the
//! whole viewport (else the fixture is not reading history), and a notch must
//! materialize exactly the rows that moved in.
//!
//! Key fields covered: the memo hit and store (all fixtures), `content_gen`
//! (`output_arriving_…`), `renumber` (`a_kitty_unscroll_…`) and `visible_rows`
//! (all fixtures, through slot-table collisions). `cols` is an equivalent
//! mutant: a width change reflows history and bumps `content_gen` anyway; it
//! stays in the key as defence for a future resize path.

use aterm_core::render::RenderInput;
use aterm_core::terminal::Terminal;
use aterm_grid::test_counters::take_viewport_row_materialize;
use aterm_scrollback::Scrollback;

const ROWS: u16 = 24;
const COLS: u16 = 80;

/// Deep enough that the whole viewport is history at [`DEPTH`], and deep enough
/// that the rows read live below the ring's newest lines.
const FILL: usize = 2_000;

/// Where the scrolled-back arm parks: an ordinary "scrolled up to read the
/// build output" distance, and comfortably more than [`ROWS`] so not one live
/// row is on screen.
const DEPTH: i32 = 200;

/// One wheel notch, in lines.
const NOTCH: i32 = 3;

fn filled_terminal() -> Terminal {
    let mut term = Terminal::new(ROWS, COLS);
    let mut corpus = String::with_capacity(FILL * 24);
    for i in 0..FILL {
        // Wide enough to be a real row, not a one-cell degenerate case.
        corpus.push_str(&format!("sb {i:05} the quick brown fox jumps over it\r\n"));
    }
    term.process(corpus.as_bytes());
    term
}

/// [`filled_terminal`]'s twin over a tiered scrollback store, for the one
/// fixture that needs a Kitty unscroll: that path budgets against the tiered
/// line count and no-ops on a ring-only grid.
fn filled_tiered_terminal() -> Terminal {
    // Ring small enough that the corpus lands in the tiers; tier limits and
    // budget wide enough to retain all of FILL.
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

    // Control: at the live bottom no row is history, so the count is zero
    // regardless of the memo.
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

    // Reach. The first scrolled-back frame finds a cold memo and pays for the
    // whole viewport plus one: the incoming-row apron (the row just below the
    // viewport, `Terminal::apron_row_into`) is read once per offset change and
    // lands in the memo too. Fewer than ROWS + 1 means the fixture is not
    // reading history.
    term.scroll_display(DEPTH);
    frame(&mut term, &mut scratch);
    assert_eq!(
        take_viewport_row_materialize(),
        usize::from(ROWS) + 1,
        "the first scrolled-back frame must materialize the whole viewport (plus \
         the apron row) from the tiers — if it does not, this workload never \
         reached the SCR-1 path"
    );

    // The claim. Every repaint after that, with the viewport motionless, is
    // free. This is 24-per-frame -> 0, and it is what the pill fade, the cursor
    // blink, an effects frame and a selection drag all stopped paying.
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

    // A notch brings exactly NOTCH new absolute rows into the viewport; the
    // other ROWS - NOTCH keys must hit. A memo keyed on viewport position
    // instead of row identity passes the stationary test and fails here.
    term.scroll_display(NOTCH);
    frame(&mut term, &mut scratch);
    let moved = take_viewport_row_materialize();
    assert_eq!(
        moved,
        usize::try_from(NOTCH).expect("NOTCH is positive"),
        "a {NOTCH}-line wheel notch materialized {moved} rows; it must pay for \
         the rows that scrolled IN and nothing else"
    );

    // …and the frame after the notch is free again.
    frame(&mut term, &mut scratch);
    assert_eq!(
        take_viewport_row_materialize(),
        0,
        "the repaint after a notch settles back to zero"
    );
}

/// Output arriving while scrolled back moves the history under the viewport,
/// so the next frame pays again: the memo must not serve stale glyphs.
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

/// The `history_renumber_epoch` half of the memo key, which `content_gen`
/// cannot stand in for.
///
/// A Kitty CSI `+T` unscroll removes the newest scrollback lines: older rows
/// keep their content but their absolute keys shift wholesale, and no
/// content_gen, damage or row-revision signal distinguishes that from an
/// append. Without `renumber` in `HistoryEpoch` every other test here stays
/// green.
#[test]
fn a_kitty_unscroll_renumbers_history_and_reopens_the_bill() {
    // A tiered store with a tiny ring, so most of the corpus sits in the tiers
    // (`unscroll_from_scrollback` reads the ring, then the store).
    let mut term = filled_tiered_terminal();
    let mut scratch = RenderInput::empty();
    term.scroll_display(DEPTH);
    frame(&mut term, &mut scratch);
    let _ = take_viewport_row_materialize();
    frame(&mut term, &mut scratch);
    assert_eq!(take_viewport_row_materialize(), 0, "parked and warm");

    // Reach, four ways — this fixture only proves anything while all of these
    // hold, and three of them are the exact conditions that make the renumber
    // epoch the sole authority.
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
    // The viewport keeps its depth across the unscroll, so the same 24 absolute
    // keys are asked for again — while the line each one names has shifted by
    // the three the unscroll pulled back onto the screen. Same keys, different
    // rows: the collision `history_renumber_epoch` exists to prevent.
    assert_eq!(
        term.grid().display_offset(),
        usize::try_from(DEPTH).expect("DEPTH is positive"),
        "the viewport left its parking depth, so the next frame asks for a \
         different key set and would miss for reasons of its own"
    );

    // Attribute the count to the frame: whatever the unscroll itself read is
    // not what this fixture is claiming anything about.
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
