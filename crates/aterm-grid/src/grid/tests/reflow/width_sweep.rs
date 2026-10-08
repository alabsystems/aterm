// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Width-sweep topology tests: a rapid width sweep that returns to the original
//! column count restores the original wrap topology (same viewport and history
//! rows, prompt directly under its content).

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

/// Join the snapshot into logical lines across all boundaries (history and
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

/// Filler history, one long wrapped logical line at the bottom of the viewport,
/// then a prompt row. Returns the long line.
fn build_audit_grid(grid: &mut Grid) -> String {
    // Distinct filler lines, most scrolling into history.
    for i in 0..8u16 {
        type_line(grid, &format!("filler-{i}"));
        feed(grid);
    }
    // One long logical line wrapping across several viewport rows at 30 cols.
    let long: String = format!("{}8UtaKZtFqk", "x".repeat(60));
    type_line(grid, &long);
    feed(grid);
    type_line(grid, "$ ");
    long
}

/// The audit's rapid width sweep, ending where it began.
const SWEEP: [u16; 6] = [24, 16, 9, 13, 21, 30];

/// Run [`SWEEP`] through the synchronous resize, checking invariants each step.
fn sweep(grid: &mut Grid) {
    for w in SWEEP {
        grid.resize(6, w);
        grid.assert_invariants();
    }
}

/// A 6×30 grid over TIERED scrollback (the GUI's configuration).
fn tiered_grid() -> Grid {
    let scrollback = Scrollback::new(1000, 10_000, 10_000_000);
    Grid::with_tiered_scrollback(6, 30, 32, scrollback)
}

/// The GUI's width step: detach the tiered store, rewrap "off-thread" (here
/// inline via `reflow()`), re-attach, check invariants.
fn offloaded_resize(grid: &mut Grid, w: u16) {
    match grid.resize_offloading_scrollback(6, w) {
        Some(job) => grid.reattach_reflowed_scrollback(job.reflow()),
        None => unreachable!("tiered store must detach on a width change"),
    }
    grid.assert_invariants();
}

/// Build the audit shape in `grid`, [`sweep`] it, and require the exact
/// pre-sweep physical topology back.
fn assert_sweep_restores_wrap_topology(mut grid: Grid) {
    let long = build_audit_grid(&mut grid);

    let before = physical_snapshot(&grid);
    assert!(
        logical_lines(&before).iter().any(|l| l == &long),
        "sanity: the long line is one logical line before the sweep"
    );

    sweep(&mut grid);

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

/// THE AUDIT SWEEP: rapid width shrink then return to the original width.
/// Every physical row (history AND viewport) must match the pre-sweep
/// snapshot: identical text, identical wrapped flags — logical-line identity
/// restored, no permanent split at the history/viewport boundary, no blank
/// band between the content and the prompt.
#[test]
fn width_sweep_round_trip_restores_wrap_topology_ring() {
    assert_sweep_restores_wrap_topology(Grid::with_scrollback(6, 30, 1000));
}

/// Same sweep against TIERED scrollback (the GUI's configuration).
#[test]
fn width_sweep_round_trip_restores_wrap_topology_tiered() {
    assert_sweep_restores_wrap_topology(tiered_grid());
}

/// Mid-line spaces survive the sweep: a space at a chunk boundary is a
/// trailing blank of a continued row (outside `row.len()`), so merges and
/// Row→Line seams carry continued rows at full width.
#[test]
fn width_sweep_preserves_spaces_at_chunk_boundaries() {
    let mut grid = Grid::with_scrollback(6, 30, 1000);
    let line = "The quick brown fox jumps over the lazy dog 0123456789";
    type_line(&mut grid, line);
    feed(&mut grid);
    type_line(&mut grid, "$ ");

    sweep(&mut grid);

    let joined: String = logical_lines(&physical_snapshot(&grid)).join("\n");
    assert!(
        joined.contains(line),
        "every space must survive the sweep: {joined:?}"
    );
}

/// The GUI's resize path: each step detaches the tiered store, rewraps (inline
/// here) and re-attaches, with the deficit fill at re-attach. The settled state
/// restores the pre-sweep topology.
#[test]
fn width_sweep_round_trip_restores_wrap_topology_offloaded() {
    let mut grid = tiered_grid();
    let long = build_audit_grid(&mut grid);

    let before = physical_snapshot(&grid);

    for w in SWEEP {
        offloaded_resize(&mut grid, w);
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

/// The settled content sequence without trailing blank rows, independent of
/// the history/viewport seam.
fn settled_content(grid: &Grid) -> Vec<(String, bool)> {
    let mut snap = physical_snapshot(grid);
    while snap.last().is_some_and(|(t, w)| t.is_empty() && !w) {
        snap.pop();
    }
    snap
}

/// Offloaded width step with a scrolled-back reader: the belt lift reads
/// content rows (`row_at_screen`), so the boundary-straddling line rewraps as
/// one, matching the never-scrolled resize.
#[test]
fn offloaded_resize_while_scrolled_back_matches_unscrolled_topology() {
    let build = || {
        let mut grid = tiered_grid();
        build_audit_grid(&mut grid);
        grid
    };

    // The deep shrink pushes the long line's head into history, so its tail is
    // the viewport's leading continuation belt at the second step.
    let steps = [9u16, 26];

    // Reference: the identical grid resized while at the live bottom.
    let mut reference = build();
    for w in steps {
        offloaded_resize(&mut reference, w);
    }

    // Same grid, same steps — but the reader is scrolled back into history.
    let mut grid = build();
    grid.scroll_display(3);
    assert!(grid.display_offset() > 0, "sanity: reader is scrolled back");
    for w in steps {
        offloaded_resize(&mut grid, w);
    }
    grid.scroll_to_bottom();

    assert_eq!(
        settled_content(&grid),
        settled_content(&reference),
        "a scrolled-back offloaded resize must produce the same wrap \
         topology as the never-scrolled resize of the identical grid"
    );
}

/// The trailing-blank count captured at detach is counted over content rows,
/// so a scrolled-back reader does not undercount a genuine erased band (which
/// the re-attach fill would refill with history).
#[test]
fn offloaded_resize_while_scrolled_back_keeps_erased_band_blank() {
    let mut grid = tiered_grid();
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
    offloaded_resize(&mut grid, 40);
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

/// ED 2 + cursor home during the offloaded reflow window invalidates the
/// deficit fill armed at detach, so no pre-clear history returns.
#[test]
fn clear_mid_flight_does_not_resurrect_history_offloaded() {
    let mut grid = tiered_grid();
    build_audit_grid(&mut grid);

    // Open the reflow window on a width change (arms the fill target).
    let job = grid
        .resize_offloading_scrollback(6, 40)
        .expect("tiered store must detach on a width change");

    // Mid-flight: the program clears the screen and homes the cursor
    // (`clear`'s ED 2 + CUP) — not an ED 3, so history itself survives.
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

/// An early-wrapped wide char leaves its last-column cell unwritten; the grow
/// merge joins the halves without padding that hole.
#[test]
fn early_wrap_pad_preserves_real_trailing_spaces() {
    use crate::{CellFlags, StyleId};
    // Real trailing spaces sit before the one-cell early-wrap hole; the spaces
    // are content and survive the merge.
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

/// The same early-wrap hole across the history boundary: full-width
/// materialization of the ring head skips the unwritten cell.
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
    // Assert at the grown width, where a phantom would sit mid-line.
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

/// After a shrink that overflows the viewport and a grow back, the prompt sits
/// directly below its content with the viewport bottom-anchored.
#[test]
fn width_sweep_keeps_prompt_anchored_to_content() {
    let mut grid = Grid::with_scrollback(6, 30, 1000);
    build_audit_grid(&mut grid);

    let prompt_row_before = (0..grid.rows())
        .rev()
        .find(|&r| !grid.row(r).unwrap().is_empty())
        .expect("prompt row exists");

    for w in SWEEP {
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
