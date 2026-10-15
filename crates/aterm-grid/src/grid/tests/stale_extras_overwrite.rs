// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Overwriting an extras-bearing cell (hyperlink, combining marks, RGB) removes
//! its `CellExtras` entry on every grid write path, so no stale entry leaks or
//! merges into a later styled write at the same coordinate.

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
    // The bulk wide-run write runs the wide-orphan fixup once before its
    // no-fixup fast path, so a run starting on a continuation half leaves no
    // dangling WIDE base.
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

    // CUP onto the continuation half (col 1), then a bulk wide run: col 0's WIDE
    // base is cleaned up.
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
    // A new extras-bearing write on an old hyperlink cell does not inherit the
    // stale hyperlink.
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

/// Defence in depth (r15 C2): an extras run that creates map entries (an
/// underline colour, OSC 8) starts each cell's entry fresh, so it never adopts
/// an entry its cells did not claim, whatever left it there. The orphan is
/// planted straight into the map, as the double-width ICH/DCH left one. A
/// truecolor-only run writes the ring, not the map, and relies on orphans
/// being closed where they are made (ruling 2026-10-11 in
/// `CellExtras::set_range_uniform`), which the process fuzzers assert.
#[test]
fn an_extras_run_never_adopts_an_orphaned_entry() {
    let url: Arc<str> = Arc::from("https://example.com/new");
    // (pen, underline colour, link)
    let pens = [
        ("underline colour", Some(0x01_FF_00_00), None),
        ("hyperlink", None, Some(&url)),
    ];
    for (pen, underline, link) in pens {
        let mut grid = Grid::new(5, 30);
        let orphan = CellCoord::new(0, 2);
        grid.extras_mut()
            .get_or_create(orphan)
            .add_combining('\u{0301}');
        grid.extras_mut()
            .get_or_create(orphan)
            .set_hyperlink(Some(Arc::from("https://example.com/old")));
        assert!(!grid.cell(0, 2).unwrap().has_extras(), "planted unclaimed");

        grid.set_cursor(0, 0);
        let mut last = None;
        grid.write_ascii_run_with_extras(
            b"hello",
            PackedColors::new(),
            CellFlags::empty(),
            None,
            None,
            underline,
            0,
            link,
            None,
            &mut last,
        );

        assert_eq!(grid.row_text(0).as_deref(), Some("hello"), "{pen}");
        let extra = grid.cell_extra(0, 2).expect("the run's own entry");
        assert!(
            extra.combining().is_empty(),
            "{pen}: the run adopted the orphan's mark: {extra:?}"
        );
        assert_eq!(
            extra.hyperlink().map(|u| &**u),
            link.map(|u| &**u),
            "{pen}: the run adopted the orphan's link"
        );
        grid.assert_extras_claimed();
    }
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

/// The bulk wide-run lane tightens `len` like the single-glyph writer and
/// `fill_cell_run`: a run starting on a continuation orphans that char's base,
/// and its tail can orphan the continuation just past its end; when that orphan
/// was the row's last content, `len` shrinks (`Row::update_len` only grows).
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

    // Cursor on col 1 (the continuation of `日`), then a two-glyph run: it
    // orphans `日`'s base at col 0 and `語`'s continuation at col 5, the last
    // content.
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

/// The same obligation on the emoji run lane. Asserted on the trailing space:
/// non-BMP glyphs render as U+FFFD on a bare `Grid`, and a stale `len` shows
/// as exactly one trailing blank.
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

    // Cursor on the first emoji's continuation (col 1), then a two-glyph run
    // whose end orphans the third emoji's continuation at col 5, the last content.
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
