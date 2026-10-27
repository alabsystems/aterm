// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Wide-pair orphan fixups across the row write and erase lanes.

use super::super::*;
use super::make_row;

#[test]
fn cells_mut_with_fixup_handles_multiple_wide_chars() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_wide_char(0, '\u{4E00}', fg, bg, CellFlags::empty());
    row.write_wide_char(2, '\u{4E8C}', fg, bg, CellFlags::empty());
    row.write_wide_char(4, '\u{4E09}', fg, bg, CellFlags::empty());

    {
        let target = row
            .cells_mut_with_fixup(1, 4, Cell::EMPTY)
            .expect("range inside row should produce a slice");
        assert_eq!(target.len(), 4);
    }

    assert_eq!(row.get(0).unwrap().char(), ' ');
    assert!(!row.get(0).unwrap().is_wide());

    assert_eq!(row.get(5).unwrap().char(), ' ');
    assert!(!row.get(5).unwrap().is_wide_continuation());
}

#[test]
fn cells_mut_with_fixup_ignores_out_of_bounds_start_without_overflow() {
    let mut pages = PageStore::new();
    // SAFETY: Test-local `pages` outlives `row` for the full scope.
    let mut row = unsafe { Row::new(4, &mut pages) };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(
            row.cells_mut_with_fixup(u16::MAX, 1, Cell::EMPTY).is_none(),
            "out-of-bounds start should return None"
        );
    }));

    assert!(
        result.is_ok(),
        "out-of-bounds start should return early without overflow panic"
    );
    assert_eq!(row.len(), 0);
}

#[test]
fn fixup_wide_boundary_clears_orphaned_continuation_at_left() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_wide_char(2, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(2).unwrap().is_wide());
    assert!(row.get(3).unwrap().is_wide_continuation());

    *row.get_mut(2).unwrap() = crate::Cell::with_style(
        'A',
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    assert!(!row.get(2).unwrap().is_wide());
    assert!(row.get(3).unwrap().is_wide_continuation());

    row.fixup_wide_boundary(3, 6, 10, true);

    assert!(
        !row.get(3).unwrap().is_wide_continuation(),
        "orphaned WIDE_CONTINUATION at left boundary should be cleared"
    );
    assert_eq!(
        row.get(3).unwrap().char(),
        ' ',
        "cleared cell should be empty (space)"
    );
}

#[test]
fn clear_range_with_does_not_bleed_fill_onto_left_wide_orphan() {
    use crate::{Cell, CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_wide_char(0, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(0).unwrap().is_wide());

    let fill = Cell::with_style(
        'X',
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    assert!(
        !fill.is_empty(),
        "fill must be visible for this test to mean anything"
    );

    row.clear_range_with(1, 5, fill);

    assert_eq!(
        row.get(3).unwrap().char(),
        'X',
        "in-rect cell should hold the fill"
    );
    assert!(
        !row.get(0).unwrap().is_wide(),
        "orphaned WIDE head must be cleared"
    );
    assert_eq!(
        row.get(0).unwrap().char(),
        ' ',
        "left-of-rect orphan must be EMPTY, not the fill glyph"
    );
    assert!(
        row.get(0).unwrap().is_empty(),
        "left-of-rect orphan must not inherit fill attributes"
    );
}

#[test]
fn clear_range_with_does_not_bleed_fill_onto_right_wide_orphan() {
    use crate::{Cell, CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_wide_char(6, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(7).unwrap().is_wide_continuation());

    let fill = Cell::with_style(
        'X',
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );

    row.clear_range_with(2, 7, fill);

    assert_eq!(
        row.get(6).unwrap().char(),
        'X',
        "in-rect WIDE head slot should hold the fill"
    );
    assert!(
        !row.get(7).unwrap().is_wide_continuation(),
        "orphaned continuation must be cleared"
    );
    assert_eq!(
        row.get(7).unwrap().char(),
        ' ',
        "right-of-rect orphan must be EMPTY, not the fill glyph"
    );
    assert!(
        row.get(7).unwrap().is_empty(),
        "right-of-rect orphan must not inherit fill attributes"
    );
    assert_eq!(
        row.len(),
        7,
        "len must not keep counting the cleared trailing wide orphan"
    );
}

#[test]
fn insert_chars_bounded_fill_clears_phantom_continuation() {
    use crate::{Cell, CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_wide_char(4, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(4).unwrap().is_wide());
    assert!(row.get(5).unwrap().is_wide_continuation());

    row.insert_chars_bounded_fill(5, 1, 9, Cell::EMPTY);

    assert!(
        !row.get(4).unwrap().is_wide(),
        "orphaned WIDE head at 4 must be cleared"
    );
    assert!(
        !row.get(6)
            .unwrap()
            .flags()
            .contains(CellFlags::WIDE_CONTINUATION),
        "cells[6] must not carry a stale WIDE_CONTINUATION bit"
    );
    assert!(
        !row.is_cell_protected(6),
        "cells[6] must not read as phantom-protected"
    );
    assert_eq!(
        row.get(6).unwrap().char(),
        ' ',
        "cells[6] should be a clean blank"
    );
}

#[test]
fn clear_range_with_orphan_paints_left_wide_orphan_with_the_bce_blank() {
    use crate::{Cell, CellFlags, PackedColor, PackedColors};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_wide_char(0, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(0).unwrap().is_wide());

    let fill = Cell::bce_blank(PackedColors::with_indexed_bg(4));
    assert!(!fill.is_empty(), "precondition: the BCE blank is visible");

    row.clear_range_with_orphan(1, 5, fill, fill);

    let orphan = row.get(0).unwrap();
    assert!(!orphan.is_wide(), "the orphaned WIDE head must be blanked");
    assert_eq!(orphan.char(), ' ', "the orphan becomes a space");
    assert_eq!(
        orphan.colors(),
        fill.colors(),
        "the orphan must carry the BCE background, not default colours \
         (xterm ClearRight -> ClearCells writes xtermColorPair for it)"
    );
    assert!(
        !orphan.is_empty(),
        "a default-coloured orphan is the bug: it renders as a hole in the BCE run"
    );
}

#[test]
fn clear_range_with_orphan_paints_right_wide_orphan_with_the_bce_blank() {
    use crate::{Cell, CellFlags, PackedColor, PackedColors};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_wide_char(6, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(7).unwrap().is_wide_continuation());
    assert_eq!(row.len(), 8, "precondition: the wide pair is the tail");

    let fill = Cell::bce_blank(PackedColors::with_indexed_bg(4));
    let written = row.clear_range_with_orphan(2, 7, fill, fill);

    let orphan = row.get(7).unwrap();
    assert!(
        !orphan.is_wide_continuation(),
        "the orphaned continuation must be blanked"
    );
    assert_eq!(
        orphan.colors(),
        fill.colors(),
        "the right orphan must carry the BCE background too"
    );
    assert_eq!(
        row.len(),
        8,
        "the BCE-blanked orphan at col 7 is visible content — len must cover it \
         (the #7522 shrink applies only to an EMPTY orphan)"
    );
    assert_eq!(
        written,
        2..8,
        "the written span must report the orphan column so the caller can \
         extend the truecolor side table to it"
    );
}

#[test]
fn clear_range_with_keeps_rect_orphans_empty_under_a_visible_fill() {
    use crate::{Cell, CellFlags, PackedColor, PackedColors};

    let (_pages, mut row) = make_row(12);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_wide_char(2, '\u{4E2D}', fg, bg, CellFlags::empty());
    row.write_wide_char(6, '\u{4E2D}', fg, bg, CellFlags::empty());

    let fill = Cell::bce_blank(PackedColors::with_indexed_bg(4));
    let written = row.clear_range_with_orphan(3, 7, fill, Cell::EMPTY);

    assert!(
        row.get(2).unwrap().is_empty(),
        "#7522: the left orphan of a rect op stays Cell::EMPTY"
    );
    assert!(
        row.get(7).unwrap().is_empty(),
        "#7522: the right orphan of a rect op stays Cell::EMPTY"
    );
    assert_eq!(written, 2..8, "the widened span is reported either way");
}

#[test]
fn clear_range_with_empty_fill_shrinks_len_on_tail_wide_orphan() {
    use crate::{Cell, CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    row.write_char(0, 'A');
    row.write_char(1, 'B');
    row.write_wide_char(
        2,
        '\u{4E2D}',
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    assert_eq!(row.len(), 4, "AB中 → len 4");

    row.clear_range_with(0, 3, Cell::EMPTY);

    for c in 0..4 {
        assert!(row.get(c).unwrap().is_empty(), "cell {c} should be empty");
    }
    assert_eq!(
        row.len(),
        0,
        "len must shrink to 0 when the whole row is erased"
    );
}

#[test]
fn selective_clear_range_shrinks_len_on_tail_wide_orphan() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    row.write_wide_char(
        4,
        '\u{4E2D}',
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    assert_eq!(row.len(), 6, "wide char at 4-5 → len 6");

    row.selective_clear_range(0, 5, Cell::EMPTY);

    for c in 0..6 {
        assert!(row.get(c).unwrap().is_empty(), "cell {c} should be empty");
    }
    assert_eq!(
        row.len(),
        0,
        "len must shrink to 0 after erasing the trailing wide char"
    );
}

#[test]
fn selective_wipe_range_shrinks_len_on_tail_wide_orphan() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    row.write_wide_char(
        4,
        '\u{4E2D}',
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    assert_eq!(row.len(), 6, "wide char at 4-5 → len 6");

    let mut wiped = Vec::new();
    row.selective_wipe_range(0, 5, &mut wiped);

    for c in 0..6 {
        assert!(
            row.get(c).unwrap().is_empty(),
            "wiped default-colored cell {c} should read empty"
        );
    }
    assert_eq!(
        row.len(),
        0,
        "len must shrink to 0 after wiping the trailing wide char"
    );
}

#[test]
fn write_char_shrinks_len_on_tail_wide_head_overwrite() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    row.write_wide_char(
        8,
        '\u{4E2D}',
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    assert_eq!(row.len(), 10, "wide char at 8-9 → len 10");

    row.write_char(8, 'X');
    assert_eq!(row.get(8).unwrap().char(), 'X');
    assert!(
        row.get(9).unwrap().is_empty(),
        "orphaned continuation at col 9 must be empty"
    );
    assert_eq!(
        row.len(),
        9,
        "len must shrink to 9 (no phantom trailing space)"
    );
}

#[test]
fn write_char_packed_shrinks_len_on_tail_wide_head_overwrite() {
    use crate::{CellFlags, PackedColor, PackedColors};

    let (_pages, mut row) = make_row(10);
    row.write_wide_char(
        8,
        '\u{4E2D}',
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    assert_eq!(row.len(), 10, "wide char at 8-9 → len 10");

    assert!(row.write_char_packed(8, 'X', PackedColors::DEFAULT, CellFlags::empty()));
    assert!(
        row.get(9).unwrap().is_empty(),
        "orphaned continuation at col 9 must be empty"
    );
    assert_eq!(
        row.len(),
        9,
        "len must shrink to 9 (no phantom trailing space)"
    );
}

#[test]
fn write_wide_char_packed_shrinks_len_on_tail_wide_orphan() {
    use crate::{CellFlags, PackedColors};

    let (_pages, mut row) = make_row(4);
    row.write_wide_char_packed(2, '\u{4E00}', PackedColors::DEFAULT, CellFlags::empty());
    assert_eq!(row.len(), 4, "wide char at 2-3 → len 4");

    assert!(row.write_wide_char_packed(1, '\u{4E8C}', PackedColors::DEFAULT, CellFlags::empty()));
    assert!(row.get(1).unwrap().is_wide());
    assert!(row.get(2).unwrap().is_wide_continuation());
    assert!(
        row.get(3).unwrap().is_empty(),
        "orphaned continuation at col 3 must be empty"
    );
    assert_eq!(
        row.len(),
        3,
        "len must shrink to 3 (no phantom trailing space)"
    );
}

#[test]
fn fill_cell_run_shrinks_len_on_tail_wide_orphan() {
    use crate::{Cell, CellFlags, PackedColor};

    let (_pages, mut row) = make_row(20);
    row.write_wide_char(
        5,
        '\u{4E2D}',
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    assert_eq!(row.len(), 7, "wide char at 5-6 → len 7");

    let written = row.fill_cell_run(3, 3, Cell::from_ascii_fast(b'X'));
    assert_eq!(written, 3);
    assert!(
        row.get(6).unwrap().is_empty(),
        "orphaned continuation at col 6 must be empty"
    );
    assert_eq!(
        row.get(5).unwrap().char(),
        'X',
        "last filled cell holds the template"
    );
    assert_eq!(
        row.len(),
        6,
        "len must shrink to 6 (content ends at the fill's right edge)"
    );
}

#[test]
fn cells_mut_with_fixup_shrinks_len_on_tail_wide_orphan() {
    use crate::{Cell, CellFlags, PackedColor};

    let (_pages, mut row) = make_row(20);
    row.write_wide_char(
        5,
        '\u{4E2D}',
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    assert_eq!(row.len(), 7, "wide char at 5-6 → len 7");

    {
        let target = row
            .cells_mut_with_fixup(3, 3, Cell::EMPTY)
            .expect("range inside row");
        assert_eq!(target.len(), 3);
        for (i, cell) in target.iter_mut().enumerate() {
            *cell = Cell::from_ascii_fast(b'A' + i as u8);
        }
    }
    row.update_len(3 + 3); // mirror the bulk callers' grow-only len update

    assert!(
        row.get(6).unwrap().is_empty(),
        "orphaned continuation at col 6 must be empty"
    );
    assert_eq!(
        row.get(5).unwrap().char(),
        'C',
        "last written cell holds the ASCII byte"
    );
    assert_eq!(
        row.len(),
        6,
        "len must shrink to 6, not stay stale-high at 7"
    );
}

#[test]
fn delete_chars_fill_shrinks_len_on_deleted_wide_head_at_tail() {
    use crate::{Cell, CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    row.write_char(0, 'a');
    row.write_char(1, 'b');
    row.write_wide_char(
        2,
        '\u{4E2D}',
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    assert_eq!(row.len(), 4, "ab中 → len 4");

    row.delete_chars_fill(2, 1, Cell::EMPTY);

    assert_eq!(row.get(0).unwrap().char(), 'a');
    assert_eq!(row.get(1).unwrap().char(), 'b');
    assert!(
        row.get(2).unwrap().is_empty(),
        "deleted region must be empty"
    );
    assert_eq!(
        row.len(),
        2,
        "len must shrink to 2, not stay stale-high at 3"
    );
}

#[test]
fn an_early_wrap_hole_lasts_until_its_cell_is_written() {
    use crate::{Cell, CellFlags, PackedColor};

    let hole_row = || {
        let (pages, mut row) = make_row(4);
        row.write_char(0, 'a');
        row.set_early_wrap_hole(true);
        assert!(row.has_early_wrap_hole(), "fixture");
        (pages, row)
    };
    let (_p, mut row) = hole_row();
    assert!(!row.write_char(4, ' '), "past the row: nothing written");
    assert!(row.has_early_wrap_hole(), "a write elsewhere keeps it");
    row.write_char(3, ' ');
    assert!(!row.has_early_wrap_hole(), "a printed space is content");

    let (_p, mut row) = hole_row();
    let packed = Cell::convert_colors(PackedColor::DEFAULT_FG, PackedColor::DEFAULT_BG);
    row.write_char_packed(3, ' ', packed, CellFlags::empty());
    assert!(!row.has_early_wrap_hole(), "the packed lane");

    let (_p, mut row) = hole_row();
    row.fill_cell_run(2, 2, Cell::new(' '));
    assert!(!row.has_early_wrap_hole(), "the run lane");

    let (_p, mut row) = hole_row();
    let _ = row.cells_mut_with_fixup(1, 3, Cell::EMPTY);
    assert!(!row.has_early_wrap_hole(), "the bulk lane");

    let (_p, mut row) = hole_row();
    row.set(3, Cell::new('x'));
    assert!(!row.has_early_wrap_hole(), "a glyph in the cell is content");

    let (_p, mut row) = hole_row();
    row.set(3, Cell::new(' '));
    assert!(
        !row.has_early_wrap_hole(),
        "a blank `Row::set` puts there too"
    );

    let (mut pages, mut row) = hole_row();
    // SAFETY: `pages` outlives `row` in this test.
    unsafe { row.resize(6, &mut pages) };
    assert!(!row.has_early_wrap_hole(), "a width change without rewrap");

    let (_p, mut row) = hole_row();
    row.clear();
    assert!(!row.has_early_wrap_hole(), "a whole-row clear");
}

#[test]
fn an_early_wrap_hole_ends_when_its_cell_is_rewritten_with_a_blank() {
    use crate::{Cell, CellFlags, PackedColor};

    let packed = Cell::convert_colors(PackedColor::DEFAULT_FG, PackedColor::DEFAULT_BG);
    let hole_row = || {
        let (pages, mut row) = make_row(4);
        row.write_char(0, 'a');
        row.write_char(1, 'b');
        row.write_char(2, ' ');
        row.set_early_wrap_hole(true);
        assert!(row.has_early_wrap_hole(), "fixture");
        (pages, row)
    };

    let (_p, mut row) = hole_row();
    assert!(row.write_wide_char_packed(2, '\u{6587}', packed, CellFlags::empty()));
    row.write_char(2, 'x');
    assert_eq!(row.get(3).map(Cell::char_data), Some(u16::from(b' ')));
    assert!(
        !row.has_early_wrap_hole(),
        "the wide glyph's spacer was written there"
    );

    let (_p, mut row) = hole_row();
    // SAFETY: columns 2 and 3 exist; one glyph on a fresh boundary.
    unsafe { row.write_wide_char_packed_no_fixup(2, '\u{6587}', packed, CellFlags::empty()) };
    row.settle_len_after_wide_run(4, false);
    row.mark_has_wide_chars();
    row.write_char(2, 'x');
    assert_eq!(row.get(3).map(Cell::char_data), Some(u16::from(b' ')));
    assert!(!row.has_early_wrap_hole(), "the wide run lane");

    let (_p, mut row) = hole_row();
    row.insert_chars(0, 1);
    assert_eq!(row.get(3).map(Cell::char_data), Some(u16::from(b' ')));
    assert!(
        !row.has_early_wrap_hole(),
        "ICH shifted the printed space in"
    );

    let (_p, mut row) = hole_row();
    row.delete_chars_fill(0, 1, Cell::EMPTY);
    assert!(!row.has_early_wrap_hole(), "DCH filled the cell");

    let (_p, mut row) = hole_row();
    row.insert_chars_bounded_fill(0, 1, 4, Cell::EMPTY);
    assert!(!row.has_early_wrap_hole(), "a bounded ICH to the row end");

    let (_p, mut row) = hole_row();
    row.delete_chars_bounded_fill(0, 1, 4, Cell::EMPTY);
    assert!(!row.has_early_wrap_hole(), "a bounded DCH to the row end");

    let (_p, mut row) = hole_row();
    row.insert_chars_bounded_fill(0, 1, 3, Cell::EMPTY);
    row.delete_chars_bounded_fill(0, 1, 3, Cell::EMPTY);
    assert!(
        row.has_early_wrap_hole(),
        "a shift inside a right margin leaves the last cell alone"
    );

    let (_p, mut row) = hole_row();
    row.clear_range_with(3, 4, Cell::new('x'));
    row.clear_range_with(3, 4, Cell::new(' '));
    assert!(
        !row.has_early_wrap_hole(),
        "a DECFRA glyph was content, and erasing it leaves a blank, not a hole"
    );

    let (_p, mut row) = hole_row();
    row.clear_range_with(2, 4, Cell::new(' '));
    assert!(row.has_early_wrap_hole(), "a DECERA blank fill erases");

    let (_p, mut row) = hole_row();
    row.erase_chars(3, 1);
    assert!(row.has_early_wrap_hole(), "an erase blanks the hole again");
}
