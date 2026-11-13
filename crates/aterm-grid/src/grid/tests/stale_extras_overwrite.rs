// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for clearing `CellExtras` entries when overwriting cells.

use super::*;
use std::sync::Arc;

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
    let colors = Cell::convert_colors(PackedColor::indexed(2), PackedColor::DEFAULT_BG);
    grid.write_ascii_run_styled_packed(b"abcd", colors, CellFlags::BOLD, &mut last);

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
    let mut grid = Grid::new(5, 10);
    grid.set_cursor(0, 0);
    assert!(grid.write_wide_autowrap_fast('中', PackedColors::new(), CellFlags::empty()));
    assert!(grid.cell(0, 0).unwrap().flags().contains(CellFlags::WIDE));
    assert!(
        grid.cell(0, 1)
            .unwrap()
            .flags()
            .contains(CellFlags::WIDE_CONTINUATION)
    );

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
fn an_extras_run_never_adopts_an_orphaned_entry() {
    let url = Arc::new(crate::HyperlinkData {
        url: Arc::from("https://example.com/new"),
        id: None,
    });
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
            link.map(|u| u.url.as_ref()),
            "{pen}: the run adopted the orphan's link"
        );
        grid.assert_extras_claimed();
    }
}

#[test]
fn failed_wide_write_does_not_drop_live_extras() {
    let mut grid = Grid::new(5, 10);
    plant_hyperlink(&mut grid, 0, 9); // last column
    grid.set_cursor(0, 9);
    let ok = grid.write_wide_char_at_cursor_packed('中', PackedColors::new(), CellFlags::empty());
    assert!(!ok, "wide char must not fit at the last column");
    assert_eq!(
        grid.extras().len(),
        1,
        "failed write must not drop live extras"
    );
}

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

/// `assert_extras_claimed` judges each entry against the live screen row the map keys it by,
/// wherever the reader has scrolled.
#[test]
fn extras_claimed_is_judged_on_the_live_row_while_scrolled_back() {
    let build = || {
        let mut grid = Grid::with_scrollback(3, 10, 100);
        for c in ['A', 'B', 'C', 'D', 'E', 'F'] {
            grid.write_char(c);
            grid.line_feed();
            grid.carriage_return();
        }
        grid.write_char('Z');
        assert!(
            grid.scrollback_lines() >= 2,
            "precondition: history to scroll over"
        );
        // Live (0, 0) and live (2, 0) each claim an entry.
        plant_hyperlink_unchecked(&mut grid, 0, 0);
        plant_hyperlink_unchecked(&mut grid, 2, 0);
        grid
    };
    let mut grid = build();
    grid.assert_extras_claimed();
    grid.scroll_display(1);
    assert_eq!(grid.display_offset(), 1);
    assert!(
        grid.cell(2, 0).is_some_and(|c| !c.has_extras()),
        "precondition: the viewport's (2, 0) is the unflagged live row 1"
    );
    grid.assert_extras_claimed();
    grid.scroll_display(-1);
    grid.assert_extras_claimed();

    // Negative control: an entry on live (1, 0), whose cell does not claim it, is reported
    // scrolled back too, where the viewport's (1, 0) is live row 0 and carries the bit.
    let mut grid = build();
    grid.cell_extra_mut_preflagged(1, 0)
        .set_hyperlink(Some(Arc::from("https://orphan.example")));
    grid.scroll_display(1);
    assert!(
        grid.cell(1, 0).is_some_and(Cell::has_extras),
        "precondition: the viewport's (1, 0) carries the bit"
    );
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        grid.assert_extras_claimed();
    }));
    assert!(
        caught.is_err(),
        "an entry its live cell does not claim must be reported while scrolled back"
    );
}

/// [`plant_hyperlink`] without its one-entry count check.
fn plant_hyperlink_unchecked(grid: &mut Grid, row: u16, col: u16) {
    grid.cell_extra_mut(row, col)
        .set_hyperlink(Some(Arc::from("https://example.com")));
    assert!(grid.cell(row, col).is_some_and(Cell::has_extras));
}
