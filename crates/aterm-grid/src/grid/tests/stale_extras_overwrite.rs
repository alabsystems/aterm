// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Stale `CellExtras` entry cleanup on overwrite (#7456).
//!
//! Overwriting an extras-bearing cell (hyperlink, combining marks, RGB)
//! used to leave its HashMap entry behind: invisible to readers (gated on
//! the cell's HAS_EXTRAS flag) but leaking memory and resurfacing when a
//! later styled write's `get_or_create` landed on the same coordinate.
//! These tests pin the fix across every grid write path: after the
//! overwrite, the map entry is GONE (`extras().len()`), and the new cell
//! content is correct.

use super::*;
use std::sync::Arc;

/// Plant a hyperlink-bearing extras entry at (row, col) and verify it.
fn plant_hyperlink(grid: &mut Grid, row: u16, col: u16) {
    grid.cell_extra_mut(row, col)
        .set_hyperlink(Some(Arc::from("https://example.com")));
    assert!(
        grid.cell(row, col).unwrap().has_extras(),
        "cell_extra_mut must set HAS_EXTRAS"
    );
    assert_eq!(grid.extras().len(), 1, "entry planted");
}

#[test]
fn write_char_clears_stale_extras_entry() {
    let mut grid = Grid::new(5, 10);
    plant_hyperlink(&mut grid, 0, 0);

    grid.set_cursor(0, 0);
    grid.write_char('x');

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'x');
    assert_eq!(
        grid.extras().len(),
        0,
        "stale entry must be removed (#7456)"
    );
}

#[test]
fn write_ascii_blast_clears_stale_extras_entries() {
    let mut grid = Grid::new(5, 10);
    plant_hyperlink(&mut grid, 0, 2);
    grid.cell_extra_mut(0, 5).add_combining('\u{0301}');
    assert_eq!(grid.extras().len(), 2);

    grid.set_cursor(0, 0);
    grid.write_ascii_blast(b"ABCDEFGH");

    assert_eq!(grid.cell(0, 2).unwrap().char(), 'C');
    assert_eq!(grid.cell(0, 5).unwrap().char(), 'F');
    assert_eq!(
        grid.extras().len(),
        0,
        "blast overwrite must remove all stale entries in range (#7456)"
    );
}

#[test]
fn write_ascii_blast_preserves_extras_outside_written_range() {
    let mut grid = Grid::new(5, 10);
    plant_hyperlink(&mut grid, 0, 8); // beyond the write
    grid.cell_extra_mut(1, 0).add_combining('\u{0301}'); // other row
    assert_eq!(grid.extras().len(), 2);

    grid.set_cursor(0, 0);
    grid.write_ascii_blast(b"ABC");

    assert_eq!(
        grid.extras().len(),
        2,
        "extras not covered by the write must survive"
    );
    assert!(grid.cell_extra(0, 8).unwrap().hyperlink().is_some());
}

#[test]
fn write_ascii_run_styled_clears_stale_extras_entries() {
    let mut grid = Grid::new(5, 10);
    plant_hyperlink(&mut grid, 0, 1);

    grid.set_cursor(0, 0);
    let mut last = None;
    grid.write_ascii_run_styled(
        b"abcd",
        PackedColor::indexed(2),
        PackedColor::DEFAULT_BG,
        CellFlags::BOLD,
        &mut last,
    );

    assert_eq!(grid.cell(0, 1).unwrap().char(), 'b');
    assert_eq!(grid.extras().len(), 0, "styled run must remove stale entry");
}

#[test]
fn write_cell_run_clears_stale_extras_entries() {
    let mut grid = Grid::new(5, 10);
    plant_hyperlink(&mut grid, 0, 3);

    grid.set_cursor(0, 0);
    let mut last = None;
    grid.write_cell_run(b'=', 6, PackedColors::new(), CellFlags::empty(), &mut last);

    assert_eq!(grid.cell(0, 3).unwrap().char(), '=');
    assert_eq!(grid.extras().len(), 0, "cell run must remove stale entry");
}

#[test]
fn write_narrow_autowrap_fast_clears_stale_extras_entry() {
    let mut grid = Grid::new(5, 10);
    plant_hyperlink(&mut grid, 0, 0);

    grid.set_cursor(0, 0);
    grid.write_narrow_autowrap_fast('y', PackedColors::new(), CellFlags::empty());

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'y');
    assert_eq!(
        grid.extras().len(),
        0,
        "narrow fast path must remove stale entry"
    );
}

#[test]
fn write_wide_autowrap_fast_clears_stale_extras_on_both_halves() {
    let mut grid = Grid::new(5, 10);
    plant_hyperlink(&mut grid, 0, 0);
    grid.cell_extra_mut(0, 1).add_combining('\u{0301}');
    assert_eq!(grid.extras().len(), 2);

    grid.set_cursor(0, 0);
    assert!(grid.write_wide_autowrap_fast('中', PackedColors::new(), CellFlags::empty()));

    assert_eq!(grid.cell(0, 0).unwrap().char(), '中');
    assert_eq!(
        grid.extras().len(),
        0,
        "wide write must remove stale entries under BOTH halves (#7456)"
    );
}

#[test]
fn write_wide_run_autowrap_clears_stale_extras_entries() {
    let mut grid = Grid::new(5, 10);
    plant_hyperlink(&mut grid, 0, 2);

    grid.set_cursor(0, 0);
    grid.write_wide_run_autowrap(&['日', '本'], PackedColors::new(), CellFlags::empty());

    assert_eq!(grid.cell(0, 2).unwrap().char(), '本');
    assert_eq!(grid.extras().len(), 0, "wide run must remove stale entry");
}

#[test]
fn wide_run_autowrap_clears_orphaned_wide_half_at_run_start() {
    // REGRESSION (wide-orphan fixup): the bulk wide-run write uses a no-fixup
    // fast path. If the cursor was positioned (e.g. via CUP) onto the SECOND
    // half of a pre-existing wide char, the no-fixup write overwrites that
    // continuation while leaving the WIDE base in the previous column orphaned.
    // The run must run the wide-orphan fixup once before the bulk write so no
    // dangling WIDE base survives.
    let mut grid = Grid::new(5, 10);
    // Plant a wide char at cols 0-1: col0 = WIDE base, col1 = WIDE_CONTINUATION.
    grid.set_cursor(0, 0);
    assert!(grid.write_wide_autowrap_fast('中', PackedColors::new(), CellFlags::empty()));
    assert!(grid.cell(0, 0).unwrap().flags().contains(CellFlags::WIDE));
    assert!(
        grid.cell(0, 1)
            .unwrap()
            .flags()
            .contains(CellFlags::WIDE_CONTINUATION)
    );

    // CUP onto the continuation half (col 1), then write a wide run through the
    // bulk path. The first char lands at col 1, overwriting the continuation;
    // col 0's WIDE base must be cleaned up, not left orphaned.
    grid.set_cursor(0, 1);
    grid.write_mixed_wide_run_autowrap(
        &['🚀', '🚀'],
        PackedColors::new(),
        CellFlags::empty(),
        CellFlags::empty(),
    );

    assert!(
        !grid.cell(0, 0).unwrap().flags().contains(CellFlags::WIDE),
        "the orphaned WIDE base at col 0 must be cleared by the pre-run fixup"
    );
}

#[test]
fn write_ascii_run_with_extras_does_not_merge_stale_hyperlink() {
    // The resurrection case: a NEW extras-bearing write landing on an OLD
    // hyperlink cell used to `get_or_create` the stale entry and merge the
    // old hyperlink into the new text.
    let mut grid = Grid::new(5, 10);
    plant_hyperlink(&mut grid, 0, 0);

    grid.set_cursor(0, 0);
    let mut last = None;
    grid.write_ascii_run_with_extras(
        b"new",
        PackedColors::new(),
        CellFlags::empty(),
        None,
        None,
        Some(0x01_FF_00_00), // underline color only — forces the HashMap path
        0,
        None, // NO hyperlink in the new style
        None,
        &mut last,
    );

    assert_eq!(grid.cell(0, 0).unwrap().char(), 'n');
    let extra = grid
        .cell_extra(0, 0)
        .expect("fresh entry for underline color");
    assert!(
        extra.hyperlink().is_none(),
        "stale hyperlink must NOT be merged into the new cell (#7456)"
    );
}

#[test]
fn failed_wide_write_does_not_drop_live_extras() {
    // A wide char that cannot fit must leave the existing extras alone —
    // removal is gated on the write actually landing.
    let mut grid = Grid::new(5, 10);
    plant_hyperlink(&mut grid, 0, 9); // last column
    grid.set_cursor(0, 9);
    // Width-2 write at the last column cannot fit (effective_cols = 10).
    let ok = grid.write_wide_char_at_cursor_packed('中', PackedColors::new(), CellFlags::empty());
    assert!(!ok, "wide char must not fit at the last column");
    assert_eq!(
        grid.extras().len(),
        1,
        "failed write must not drop live extras"
    );
}

/// THE #7522 LEN CLASS, on the bulk wide-RUN lane.
///
/// A wide run whose first glyph lands on the WIDE_CONTINUATION half of an
/// existing wide char orphans that char's base, and the run's own tail can
/// orphan the continuation of the wide char just past its end. When that
/// orphan WAS the row's last content, `len` must tighten — otherwise
/// `row_text`, search and scrollback all report a phantom trailing space.
///
/// The single-glyph writer closes this in its else arm
/// (`write_wide_char_packed_shrinks_len_on_tail_wide_orphan`), and
/// `fill_cell_run` closes it too. This pins the run lane to the same contract,
/// because `Row::update_len` is grow-only and a run lane that reaches for it
/// alone reopens the class.
#[test]
fn wide_run_autowrap_tightens_len_when_the_run_orphans_the_row_tail() {
    let mut grid = Grid::new(2, 80);
    grid.set_cursor(0, 0);
    grid.write_wide_run_autowrap(&['日', '本', '語'], PackedColors::new(), CellFlags::empty());
    assert_eq!(
        grid.row_text(0).as_deref(),
        Some("日本語"),
        "three wide chars occupy cols 0..6"
    );

    // Cursor onto col 1 — the CONTINUATION half of `日` — then a two-glyph run.
    // It orphans `日`'s base at col 0, and its own end orphans `語`'s
    // continuation at col 5, which is the row's last content.
    grid.set_cursor(0, 1);
    grid.write_wide_run_autowrap(&['あ', 'い'], PackedColors::new(), CellFlags::empty());

    assert!(
        grid.cell(0, 0).unwrap().is_empty(),
        "the orphaned base of the char the run landed inside must be cleared"
    );
    assert!(
        grid.cell(0, 5).unwrap().is_empty(),
        "the orphaned continuation past the run's end must be cleared"
    );
    assert_eq!(
        grid.row_text(0).as_deref(),
        Some(" あい"),
        "len must tighten to 5 — a stale-high len paints a phantom trailing space"
    );
}

/// The SAME obligation on the EMOJI run lane, which has always used the bulk
/// no-fixup path.
///
/// Asserted on the TRAILING SPACE rather than on the glyphs: a non-BMP
/// codepoint lives in the complex-char ring, so a bare `Grid` renders these as
/// U+FFFD. The replacement chars are beside the point — whether `len` was left
/// stale-high is the point, and a stale-high `len` shows up as exactly one
/// trailing blank.
#[test]
fn emoji_run_autowrap_tightens_len_when_the_run_orphans_the_row_tail() {
    let mut grid = Grid::new(2, 80);
    grid.set_cursor(0, 0);
    grid.write_emoji_run_autowrap(
        &['\u{1F600}', '\u{1F601}', '\u{1F602}'],
        PackedColors::new(),
        CellFlags::empty(),
    );
    let three = grid.row_text(0).expect("row 0 exists");
    assert_eq!(three.chars().count(), 3, "three emoji occupy cols 0..6");

    // Cursor onto col 1 — the continuation half of the first emoji — then a
    // two-glyph run whose end orphans the third emoji's continuation at col 5,
    // which is the row's last content.
    grid.set_cursor(0, 1);
    grid.write_emoji_run_autowrap(
        &['\u{1F603}', '\u{1F604}'],
        PackedColors::new(),
        CellFlags::empty(),
    );
    assert!(
        grid.cell(0, 5).unwrap().is_empty(),
        "the orphaned continuation past the run's end must be cleared"
    );
    let after = grid.row_text(0).expect("row 0 exists");
    assert!(
        !after.ends_with(' '),
        "len must tighten — a stale-high len paints a phantom trailing space, got {after:?}"
    );
}
