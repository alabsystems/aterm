// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! SCR-1 AS A COUNT: a stationary scrolled-back repaint materializes NOTHING.
//!
//! # Why this file exists at all
//!
//! The perf campaign that produced SCR-1 landed ~20 measured wins. What
//! protects them is `xtask gate perf`, which is a TIMING gate: it needs a
//! release build of several harnesses and it is not in the merge contract,
//! because a per-push timing gate on this box cannot resolve anything smaller
//! than a couple of milliseconds (the paired-round noise floor) and a hook slow
//! enough to be bypassed teaches the bypass. So the largest wins of the campaign
//! shipped with no automatic defender.
//!
//! A COUNT has none of those problems. It is machine-independent, it is exact,
//! it cannot flake under load, and it rides `cargo test` — and therefore
//! `tools/verify.sh --fast` and the pre-push hook — at zero marginal cost. This
//! crate already had the shape (`aterm_grid::test_counters`); it had simply
//! never been pointed at the campaign's own numbers.
//!
//! WHAT A COUNT CANNOT CATCH, said plainly so nobody reads more into a green run
//! than it means: a CONSTANT-FACTOR slowdown with the counts unchanged. If
//! materializing one row gets 3x more expensive, this file stays green — the
//! frame still materializes zero rows when parked and three on a wheel notch.
//! Counts catch STRUCTURE (a memo that stopped memoizing, an O(N) walk that came
//! back, a cache key that stopped matching); the timing lanes catch cost. This
//! is a floor under the structural half, not a replacement for measurement.
//!
//! # The claim, in the numbers the commit reported
//!
//! Before SCR-1, a stationary scrolled-back viewport rebuilt all 24 visible rows
//! from the 3-tier store ON EVERY PRESENTED FRAME — the pill fade, the cursor
//! blink, an effects frame and every mouse-move of a selection drag each paid a
//! full viewport of materializations for zero new information. The live-bottom
//! control paid zero. SCR-1 made the scrolled-back frame pay zero too, and a
//! wheel notch pay only for the rows that actually scrolled in (24 -> 3).
//!
//! # Both sides, always
//!
//! A one-sided "it is zero" test passes just as well when the workload never
//! reached the path — which is how a fence ends up measuring nothing. So the
//! first scrolled-back frame is asserted to materialize the FULL viewport (the
//! memo starts cold: if that is not `ROWS`, this fixture is not actually reading
//! history and every zero below is vacuous), and a wheel notch is asserted to
//! materialize EXACTLY the rows that moved in.

use aterm_core::prelude::Terminal;
use aterm_core::render::RenderInput;
use aterm_grid::test_counters::take_viewport_row_materialize;

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

fn frame(term: &mut Terminal, scratch: &mut RenderInput) {
    term.cell_frame_into(scratch, usize::from(ROWS), usize::from(COLS));
}

#[test]
fn a_stationary_scrolled_back_repaint_materializes_no_history_rows() {
    let mut term = filled_terminal();
    let mut scratch = RenderInput::empty();

    // THE CONTROL. At the live bottom no row is history, so the count is zero
    // for a reason that has nothing to do with the memo. If this ever moves,
    // the counter is counting the wrong thing and every assertion below is
    // measuring something else.
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

    // REACH. The first scrolled-back frame finds a cold memo and must pay for
    // the whole viewport. A number below ROWS here means the fixture is not
    // reading history — and would make the zeros below prove nothing.
    term.scroll_display(DEPTH);
    frame(&mut term, &mut scratch);
    assert_eq!(
        take_viewport_row_materialize(),
        usize::from(ROWS),
        "the first scrolled-back frame must materialize the whole viewport from \
         the tiers — if it does not, this workload never reached the SCR-1 path"
    );

    // THE CLAIM. Every repaint after that, with the viewport motionless, is
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

    // A NOTCH brings exactly NOTCH new absolute rows into the viewport; the
    // other ROWS - NOTCH keys are still in the memo and must hit. This is the
    // "24 -> 3" half of SCR-1 and it is the assertion that would catch a memo
    // keyed on viewport POSITION instead of row IDENTITY — such a memo passes
    // the stationary test above and fails this one with ROWS.
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

/// The memo must not survive a CONTENT change, and the count is how that is
/// visible: output arriving while scrolled back moves the history under the
/// viewport, so the next frame has to pay again. A memo that stayed "free" here
/// would be serving stale glyphs — the failure mode SCR-1's epoch key exists to
/// make impossible, pinned from the cost side.
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
