// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Width-sweep topology tests (feel audit, fixwave5): a rapid width sweep that
//! returns to the original column count must restore the ORIGINAL wrap
//! topology — the same physical rows in the viewport, the same history rows,
//! the prompt anchored directly under its content. Before the fix, a logical
//! line whose wrapped rows straddled the history/viewport boundary mid-sweep
//! stayed permanently split at that boundary, and the viewport was padded with
//! blank rows below a mid-window prompt.

use super::super::super::*;

/// A full physical snapshot of the buffer: history rows (oldest first), then
/// viewport rows, each as (text, wrapped) with trailing blanks trimmed.
fn physical_snapshot(grid: &Grid) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for i in 0..grid.scrollback_lines() {
        let line = grid.get_history_line(i).expect("history line present");
        out.push((
            line.to_string().trim_end_matches(' ').to_string(),
            line.is_wrapped(),
        ));
    }
    for r in 0..grid.rows() {
        let row = grid.row(r).expect("visible row present");
        out.push((
            row.to_string().trim_end_matches(' ').to_string(),
            row.is_wrapped(),
        ));
    }
    out
}

/// Join the snapshot into logical lines across ALL boundaries (history and
/// viewport alike): a wrapped row continues the previous physical row.
fn logical_lines(snapshot: &[(String, bool)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (text, wrapped) in snapshot {
        if *wrapped && !out.is_empty() {
            out.last_mut().unwrap().push_str(text);
        } else {
            out.push(text.clone());
        }
    }
    out
}

/// Write `text` at the cursor with autowrap via the grid's own write path,
/// so wrapped flags are produced exactly as live output produces them.
fn type_line(grid: &mut Grid, text: &str) {
    for c in text.chars() {
        grid.write_char_wrap(c);
    }
}

fn feed(grid: &mut Grid) {
    grid.line_feed();
    grid.carriage_return();
}

/// Build the audit shape: filler history, one long wrapped logical line at the
/// bottom of the viewport, then a prompt row under it. Returns the long line.
fn build_audit_grid(grid: &mut Grid) -> String {
    // Distinct filler lines, most scrolling into history.
    for i in 0..8u16 {
        type_line(grid, &format!("filler-{i}"));
        feed(grid);
    }
    // One long logical line: wraps across several viewport rows at 30 cols.
    // Tail mirrors the audit's '...8UtaKZtFqk' searched suffix.
    let long: String = format!("{}8UtaKZtFqk", "x".repeat(60));
    type_line(grid, &long);
    feed(grid);
    type_line(grid, "$ ");
    long
}

/// THE AUDIT SWEEP: rapid width shrink then return to the original width.
/// Every physical row (history AND viewport) must match the pre-sweep
/// snapshot: identical text, identical wrapped flags — logical-line identity
/// restored, no permanent split at the history/viewport boundary, no blank
/// band between the content and the prompt.
#[test]
fn width_sweep_round_trip_restores_wrap_topology_ring() {
    let mut grid = Grid::with_scrollback(6, 30, 1000);
    let long = build_audit_grid(&mut grid);

    let before = physical_snapshot(&grid);
    assert!(
        logical_lines(&before).iter().any(|l| l == &long),
        "sanity: the long line is one logical line before the sweep"
    );

    // Rapid sweep through several widths, ending where it began.
    for w in [24u16, 16, 9, 13, 21, 30] {
        grid.resize(6, w);
        grid.assert_invariants();
    }

    let after = physical_snapshot(&grid);
    assert!(
        logical_lines(&after).iter().any(|l| l == &long),
        "logical line must survive the sweep intact: {after:#?}"
    );
    assert_eq!(
        after, before,
        "returning to the original width must restore the exact wrap topology"
    );
}

/// Same sweep against TIERED scrollback (the GUI's configuration).
#[test]
fn width_sweep_round_trip_restores_wrap_topology_tiered() {
    let scrollback = Scrollback::new(1000, 10_000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(6, 30, 32, scrollback);
    let long = build_audit_grid(&mut grid);

    let before = physical_snapshot(&grid);
    assert!(
        logical_lines(&before).iter().any(|l| l == &long),
        "sanity: the long line is one logical line before the sweep"
    );

    for w in [24u16, 16, 9, 13, 21, 30] {
        grid.resize(6, w);
        grid.assert_invariants();
    }

    let after = physical_snapshot(&grid);
    assert!(
        logical_lines(&after).iter().any(|l| l == &long),
        "logical line must survive the sweep intact: {after:#?}"
    );
    assert_eq!(
        after, before,
        "returning to the original width must restore the exact wrap topology"
    );
}

/// Mid-line SPACES survive the sweep. A space that lands at a chunk boundary
/// becomes a TRAILING blank cell of a continued row — outside `row.len()` —
/// and every len-trimmed merge/materialize seam used to drop it, eroding one
/// space per boundary per step ("foxjumpsover the lazydog" on the live
/// audit repro). The merge path and the Row→Line seams now carry a continued
/// row at its full width.
#[test]
fn width_sweep_preserves_spaces_at_chunk_boundaries() {
    let mut grid = Grid::with_scrollback(6, 30, 1000);
    let line = "The quick brown fox jumps over the lazy dog 0123456789";
    type_line(&mut grid, line);
    feed(&mut grid);
    type_line(&mut grid, "$ ");

    for w in [24u16, 16, 9, 13, 21, 30] {
        grid.resize(6, w);
        grid.assert_invariants();
    }

    let joined: String = logical_lines(&physical_snapshot(&grid)).join("\n");
    assert!(
        joined.contains(line),
        "every space must survive the sweep: {joined:?}"
    );
}

/// The GUI's actual resize path: every step detaches the tiered store, runs
/// the rewrap "off-thread" (here: inline via `reflow()`), and re-attaches.
/// The deficit fill runs at re-attach (`pending_fill_target`), so the settled
/// state must restore the exact pre-sweep topology just like the synchronous
/// path does.
#[test]
fn width_sweep_round_trip_restores_wrap_topology_offloaded() {
    let scrollback = Scrollback::new(1000, 10_000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(6, 30, 32, scrollback);
    let long = build_audit_grid(&mut grid);

    let before = physical_snapshot(&grid);

    for w in [24u16, 16, 9, 13, 21, 30] {
        match grid.resize_offloading_scrollback(6, w) {
            Some(job) => grid.reattach_reflowed_scrollback(job.reflow()),
            None => unreachable!("tiered store must detach on a width change"),
        }
        grid.assert_invariants();
    }

    let after = physical_snapshot(&grid);
    assert!(
        logical_lines(&after).iter().any(|l| l == &long),
        "logical line must survive the offloaded sweep intact: {after:#?}"
    );
    assert_eq!(
        after, before,
        "the offloaded sweep must restore the exact wrap topology at settle"
    );
}

/// Snapshot with the trailing blank rows stripped: the settled CONTENT
/// sequence, seam-agnostic (a row reads the same whether it sits in history
/// or the viewport) — what a scrolled-back resize must preserve even when the
/// deficit fill legitimately defers.
fn settled_content(grid: &Grid) -> Vec<(String, bool)> {
    let mut snap = physical_snapshot(grid);
    while snap.last().is_some_and(|(t, w)| t.is_empty() && !w) {
        snap.pop();
    }
    snap
}

/// The offloaded width step with a SCROLLED-BACK reader (fixwave5 review,
/// bug 1, belt side): the belt lift runs BEFORE `resize()` zeroes
/// `display_offset`, so it must read CONTENT rows (`row_at_screen`). Before
/// the fix the offset-mapped `row()` probes returned the scrolled view (or
/// `None` once the ring history was already lifted into the job), the belt
/// never lifted, and the boundary-straddling logical line rewrapped as TWO
/// independent documents — a wrap topology diverging from the never-scrolled
/// resize of the identical grid.
#[test]
fn offloaded_resize_while_scrolled_back_matches_unscrolled_topology() {
    let build = || {
        let scrollback = Scrollback::new(1000, 10_000, 10_000_000);
        let mut grid = Grid::with_tiered_scrollback(6, 30, 32, scrollback);
        build_audit_grid(&mut grid);
        grid
    };

    // Two steps: the deep shrink pushes the long line's head into history so
    // its tail is the viewport's leading continuation belt at the second
    // step — the state the belt lift exists for.
    let steps = [9u16, 26];

    // Reference: the identical grid resized while at the live bottom.
    let mut reference = build();
    for w in steps {
        match reference.resize_offloading_scrollback(6, w) {
            Some(job) => reference.reattach_reflowed_scrollback(job.reflow()),
            None => unreachable!("tiered store must detach on a width change"),
        }
        reference.assert_invariants();
    }

    // Same grid, same steps — but the reader is scrolled back into history.
    let mut grid = build();
    grid.scroll_display(3);
    assert!(grid.display_offset() > 0, "sanity: reader is scrolled back");
    for w in steps {
        match grid.resize_offloading_scrollback(6, w) {
            Some(job) => grid.reattach_reflowed_scrollback(job.reflow()),
            None => unreachable!("tiered store must detach on a width change"),
        }
        grid.assert_invariants();
    }
    grid.scroll_to_bottom();

    assert_eq!(
        settled_content(&grid),
        settled_content(&reference),
        "a scrolled-back offloaded resize must produce the same wrap \
         topology as the never-scrolled resize of the identical grid"
    );
}

/// The fill-target side of the same defect (fixwave5 review, bug 1): the
/// trailing-blank count captured at detach must be counted over CONTENT rows.
/// Before the fix a scrolled-back reader's offset shifted the probe window,
/// undercounting a GENUINE erased band below the cursor — and the re-attach
/// deficit fill then "restored" the missing blanks by resurrecting history
/// rows under the erase.
#[test]
fn offloaded_resize_while_scrolled_back_keeps_erased_band_blank() {
    let scrollback = Scrollback::new(1000, 10_000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(6, 30, 32, scrollback);
    for i in 0..9u16 {
        type_line(&mut grid, &format!("filler-{i}"));
        feed(&mut grid);
    }
    // A genuine erased band: cursor on viewport row 2, everything below
    // cleared (ED 0). filler-4/filler-5 remain at rows 0-1.
    grid.move_cursor_to(2, 0);
    grid.erase_to_end_of_screen();
    assert_eq!(grid.row(0).unwrap().to_string().trim_end(), "filler-4");

    // The reader scrolls back, then the width changes offloaded.
    grid.scroll_display(2);
    match grid.resize_offloading_scrollback(6, 40) {
        Some(job) => grid.reattach_reflowed_scrollback(job.reflow()),
        None => unreachable!("tiered store must detach on a width change"),
    }
    grid.assert_invariants();
    grid.scroll_to_bottom();

    // The erased band is genuine: no history may be pulled back under it.
    assert_eq!(
        grid.row(0).unwrap().to_string().trim_end(),
        "filler-4",
        "the deficit fill must not resurrect history above the erased band"
    );
    assert_eq!(grid.cursor_row(), 2, "the cursor must not be displaced");
    for r in 3..grid.rows() {
        assert!(
            grid.row(r).unwrap().is_empty(),
            "row {r} of the erased band must stay blank, found {:?}",
            grid.row(r).unwrap().to_string()
        );
    }
}

/// A screen-clearing erase DURING the offloaded reflow window (fixwave5
/// review, bug 2): ED 2 + cursor home mid-flight makes the blank screen
/// GENUINE, so the deficit fill armed at detach (`pending_fill_target`) must
/// be invalidated — before the fix only ED 3/replacement/abort cleared it,
/// and the re-attach fill resurrected pre-clear history onto the cleared
/// screen.
#[test]
fn clear_mid_flight_does_not_resurrect_history_offloaded() {
    let scrollback = Scrollback::new(1000, 10_000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(6, 30, 32, scrollback);
    build_audit_grid(&mut grid);

    // Open the reflow window on a width change (arms the fill target).
    let job = grid
        .resize_offloading_scrollback(6, 40)
        .expect("tiered store must detach on a width change");

    // Mid-flight: the program clears the screen and homes the cursor
    // (`clear`'s ED 2 + CUP) — NOT an ED 3, so history itself survives.
    grid.erase_screen();
    grid.move_cursor_to(0, 0);

    grid.reattach_reflowed_scrollback(job.reflow());
    grid.assert_invariants();

    // The cleared screen stays cleared: the deficit fill must not pull
    // pre-clear history back into the viewport.
    for r in 0..grid.rows() {
        assert!(
            grid.row(r).unwrap().is_empty(),
            "row {r} must stay blank after a mid-flight ED 2, found {:?}",
            grid.row(r).unwrap().to_string()
        );
    }
    // ED 2 erases only the screen — the history itself is intact.
    assert!(
        grid.scrollback_lines() > 0,
        "ED 2 must not erase the re-attached history"
    );
}

/// An early-wrapped wide char (fixwave5 review, bug 3): a CJK/emoji glyph
/// that cannot start at the last column wraps EARLY, leaving that cell
/// unwritten. The grow merge must join the halves WITHOUT padding the hole —
/// before the fix the merge pad materialized it as a phantom space right
/// before the wide char.
#[test]
fn early_wrap_pad_preserves_real_trailing_spaces() {
    use crate::{CellFlags, StyleId};
    // The re-review's deterministic repro: REAL trailing spaces sit before the
    // early-wrap hole. The hole is exactly ONE cell; the spaces are content
    // and must survive the merge.
    let mut grid = Grid::new(5, 10);
    type_line(&mut grid, "ABCDEF   "); // cols 0..=8: text + three real spaces
    grid.write_wide_char_wrap_with_style_id('世', StyleId::default(), CellFlags::empty());
    assert!(grid.row(1).unwrap().is_wrapped());
    assert!(grid.row(1).unwrap().get(0).unwrap().is_wide());

    grid.resize(5, 20);
    grid.assert_invariants();
    let row0 = grid.row(0).unwrap();
    for col in 6..9 {
        assert_eq!(
            row0.get(col).unwrap().char(),
            ' ',
            "real trailing space at col {col} must survive the merge, got {:?}",
            row0.to_string()
        );
    }
    assert!(
        row0.get(9).unwrap().is_wide(),
        "the wide char follows the three REAL spaces — only the single \
         early-wrap hole vanishes, got {:?}",
        row0.to_string()
    );
}

#[test]
fn grow_merges_early_wrapped_wide_char_without_phantom_space() {
    use crate::{CellFlags, StyleId};
    let mut grid = Grid::new(5, 10);
    // Cols 0..=8 filled; the wide char cannot start at col 9 — it early-wraps
    // and col 9 stays unwritten.
    type_line(&mut grid, "ABCDEFGHI");
    grid.write_wide_char_wrap_with_style_id('世', StyleId::default(), CellFlags::empty());
    assert!(
        grid.row(1).unwrap().is_wrapped(),
        "sanity: the wide char early-wrapped onto a continuation row"
    );
    assert!(grid.row(1).unwrap().get(0).unwrap().is_wide());

    grid.resize(5, 20);
    grid.assert_invariants();

    let row0 = grid.row(0).unwrap();
    assert!(
        row0.get(9).unwrap().is_wide(),
        "the wide char must sit directly after 'I' — the early-wrap hole is \
         not content, got {:?}",
        row0.to_string()
    );

    // Shrinking back reproduces the original early-wrap topology.
    grid.resize(5, 10);
    grid.assert_invariants();
    assert!(grid.row(1).unwrap().is_wrapped());
    assert!(
        grid.row(1).unwrap().get(0).unwrap().is_wide(),
        "the round trip must restore the early wrap, got {:?}",
        grid.row(1).unwrap().to_string()
    );
}

/// The same early-wrap hole across the HISTORY boundary (fixwave5 review,
/// bug 3, Row→Line seams): when the early-wrapped head sits in ring history
/// and its wide-char tail is the viewport's top row, the full-width
/// materialization must skip the unwritten cell too.
#[test]
fn early_wrapped_wide_char_survives_sweep_across_history_boundary() {
    use crate::{CellFlags, StyleId};
    let mut grid = Grid::with_scrollback(3, 10, 100);
    type_line(&mut grid, "ABCDEFGHI");
    grid.write_wide_char_wrap_with_style_id('世', StyleId::default(), CellFlags::empty());
    // Scroll the head into ring history; the wide-char tail stays at the
    // viewport top.
    feed(&mut grid);
    feed(&mut grid);
    assert!(grid.scrollback_lines() >= 1, "sanity: head is in history");
    assert!(
        grid.row(0).unwrap().is_wrapped(),
        "sanity: the tail is the boundary-straddling continuation"
    );

    grid.resize(3, 20);
    grid.assert_invariants();
    // Assert at the GROWN width, where the hole would sit mid-line: a shrink
    // would re-split before the wide char and hide a phantom as a trimmed
    // trailing space.
    let joined = logical_lines(&physical_snapshot(&grid)).join("\n");
    assert!(
        joined.contains("ABCDEFGHI世"),
        "no phantom space may appear before the early-wrapped wide char: {joined:?}"
    );

    // And the shrink back restores the early-wrap topology.
    grid.resize(3, 10);
    grid.assert_invariants();
    let joined = logical_lines(&physical_snapshot(&grid)).join("\n");
    assert!(
        joined.contains("ABCDEFGHI世"),
        "the round trip must keep the logical line intact: {joined:?}"
    );
}

/// The stranded-prompt anchor: after a shrink that overflows the viewport and
/// a grow back, the prompt must sit directly below its content — the viewport
/// must not hold a band of blank rows between content and prompt, and the
/// bottom of the viewport must stay anchored to the newest content.
#[test]
fn width_sweep_keeps_prompt_anchored_to_content() {
    let mut grid = Grid::with_scrollback(6, 30, 1000);
    build_audit_grid(&mut grid);

    let prompt_row_before = (0..grid.rows())
        .rev()
        .find(|&r| !grid.row(r).unwrap().is_empty())
        .expect("prompt row exists");

    for w in [24u16, 16, 9, 13, 21, 30] {
        grid.resize(6, w);
    }

    let prompt_row_after = (0..grid.rows())
        .rev()
        .find(|&r| !grid.row(r).unwrap().is_empty())
        .expect("prompt row exists after sweep");
    assert_eq!(
        grid.row(prompt_row_after).unwrap().to_string().trim_end(),
        "$",
        "the prompt is the bottom-most non-empty viewport row"
    );
    assert_eq!(
        prompt_row_after, prompt_row_before,
        "prompt must return to its pre-sweep viewport row, not strand mid-window"
    );
    // No blank row may separate the prompt from the content above it.
    assert!(
        prompt_row_after == 0 || !grid.row(prompt_row_after - 1).unwrap().is_empty(),
        "no blank band between content and prompt"
    );
}
