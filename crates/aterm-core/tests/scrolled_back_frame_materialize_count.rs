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
//!
//! # PER-SITE COVERAGE, CHECKED BY MUTATION
//!
//! "Delete the memo and watch this go red" is only honest one site at a time.
//! SCR-1 is a memo hit, a memo store, and a FOUR-FIELD epoch key, and each was
//! mutated alone:
//!
//! | site | mutated alone | caught by |
//! |------|---------------|-----------|
//! | the memo HIT (`lookup` early-return) | 24/frame returns | all fixtures |
//! | the memo STORE | 24/frame returns | all fixtures |
//! | epoch `content_gen` | stale hits after output | `output_arriving_…` |
//! | epoch `renumber` | stale hits after a Kitty unscroll | `a_kitty_unscroll_…` (added for exactly this — it was UNCOVERED) |
//! | epoch `visible_rows` | permanent misses | all fixtures, but MECHANICALLY: `capacity_for(0)` clamps the slot table to 8 for a 24-row viewport, so one viewport's keys collide. It is not a staleness gate. |
//! | epoch `cols` | NOTHING | nothing — see below |
//!
//! `cols` is not a coverage hole, it is an EQUIVALENT MUTANT: a width change
//! reflows history and `reflow.rs` bumps `content_gen` by hand, so the key is
//! already invalidated without it. `viewport_row_cache`'s own docs say `cols`
//! and `visible_rows` are in the key "so the cache cannot be wrong even if some
//! future resize path forgets to" — defence against a path that does not exist
//! yet, which no test today can distinguish from its absence. Recorded here so
//! the next reader does not mistake it for an untested field and delete it.

use aterm_core::prelude::Terminal;
use aterm_core::render::RenderInput;
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

/// [`filled_terminal`]'s twin over a TIERED scrollback store, for the one
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

/// SITE COVERAGE — the `history_renumber_epoch` half of the memo key, which
/// `content_gen` cannot stand in for.
///
/// A Kitty CSI `+T` unscroll removes the NEWEST scrollback lines. Every older
/// row keeps its content, but its ABSOLUTE key shifts wholesale, and
/// `scroll_unscroll.rs` says in so many words that "no content_gen / damage /
/// absolute-row-revision signal distinguishes this wholesale renumbering from an
/// ordinary append batch". `history_renumber_epoch` is therefore the ONLY thing
/// between the memo and a viewport of rows served under someone else's number.
///
/// It was uncovered. Dropping `renumber` from `HistoryEpoch` — measured, one
/// field at a time — left every other assertion in this file green: the
/// stationary test never renumbers, the notch test only moves the viewport, and
/// `content_gen` is what `output_arriving_…` moves. A key field nothing
/// exercises is a key field a refactor deletes.
#[test]
fn a_kitty_unscroll_renumbers_history_and_reopens_the_bill() {
    // A TIERED store, unlike the other fixtures: `unscroll_from_scrollback`
    // measures its budget against the TIERED line count and routes a ring-only
    // grid to a plain region scroll that removes nothing. A tiny ring pushes
    // the corpus down into the tiers where the unscroll can reach it.
    let mut term = filled_tiered_terminal();
    let mut scratch = RenderInput::empty();
    term.scroll_display(DEPTH);
    frame(&mut term, &mut scratch);
    let _ = take_viewport_row_materialize();
    frame(&mut term, &mut scratch);
    assert_eq!(take_viewport_row_materialize(), 0, "parked and warm");

    // REACH, four ways — this fixture only proves anything while ALL of these
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
         renumber epoch at all. A RING-ONLY grid routes here to a plain region \
         scroll; the tiered store above is what makes the unscroll real.",
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
    // The viewport keeps its depth across the unscroll, so the SAME 24 absolute
    // keys are asked for again — while the line each one names has shifted by
    // the three the unscroll pulled back onto the screen. Same keys, different
    // rows: the collision `history_renumber_epoch` exists to prevent.
    assert_eq!(
        term.grid().display_offset(),
        usize::try_from(DEPTH).expect("DEPTH is positive"),
        "the viewport left its parking depth, so the next frame asks for a \
         different key set and would miss for reasons of its own"
    );

    // Attribute the count to the FRAME: whatever the unscroll itself read is
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
