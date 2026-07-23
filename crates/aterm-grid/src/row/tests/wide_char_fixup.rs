// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates
//
// cells_mut_with_fixup behavioral tests.

use super::super::*;
use super::make_row;

#[test]
fn cells_mut_with_fixup_clears_orphaned_continuation() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Place a wide char at cols 0-1
    row.write_wide_char(0, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(0).unwrap().is_wide());
    assert!(row.get(1).unwrap().is_wide_continuation());

    // Request mutable access starting at the continuation. The helper should
    // clear the orphaned leading half before exposing the slice.
    {
        let target = row
            .cells_mut_with_fixup(1, 3)
            .expect("range inside row should produce a slice");
        assert_eq!(target.len(), 3);
    }
    assert_eq!(row.get(0).unwrap().char(), ' ');
    assert!(!row.get(0).unwrap().is_wide());
}

#[test]
fn cells_mut_with_fixup_clears_wide_continuation_past_range() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Place a wide char at cols 4-5
    row.write_wide_char(4, '\u{4E2D}', fg, bg, CellFlags::empty());

    // The helper range covers col 4 (the WIDE cell) but not col 5
    // (continuation), so col 5 should be cleared as orphaned.
    {
        let target = row
            .cells_mut_with_fixup(3, 2)
            .expect("range inside row should produce a slice");
        assert_eq!(target.len(), 2);
    }
    assert_eq!(row.get(5).unwrap().char(), ' ');
    assert!(!row.get(5).unwrap().is_wide_continuation());
}

#[test]
fn cells_mut_with_fixup_is_noop_without_wide_chars() {
    let (_pages, mut row) = make_row(10);
    for i in 0..5 {
        row.write_char(i, 'A');
    }

    {
        let target = row
            .cells_mut_with_fixup(0, 5)
            .expect("range inside row should produce a slice");
        assert_eq!(target.len(), 5);
    }
    for i in 0..5 {
        assert_eq!(row.get(i).unwrap().char(), 'A');
    }
}

#[test]
fn cells_mut_with_fixup_allows_zero_count() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_wide_char(0, '\u{4E2D}', fg, bg, CellFlags::empty());

    {
        let target = row
            .cells_mut_with_fixup(1, 0)
            .expect("zero-length range inside row should return an empty slice");
        assert!(target.is_empty());
    }
    assert!(row.get(0).unwrap().is_wide());
    assert!(row.get(1).unwrap().is_wide_continuation());
}

#[test]
fn cells_mut_with_fixup_returns_none_when_start_past_end() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    row.write_wide_char(0, '\u{4E2D}', fg, bg, CellFlags::empty());

    assert!(
        row.cells_mut_with_fixup(11, 5).is_none(),
        "start past row should return None"
    );
    assert!(row.get(0).unwrap().is_wide());
    assert!(row.get(1).unwrap().is_wide_continuation());
}

#[test]
fn cells_mut_with_fixup_handles_multiple_wide_chars() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Place wide chars at 0-1, 2-3, 4-5
    row.write_wide_char(0, '\u{4E00}', fg, bg, CellFlags::empty());
    row.write_wide_char(2, '\u{4E8C}', fg, bg, CellFlags::empty());
    row.write_wide_char(4, '\u{4E09}', fg, bg, CellFlags::empty());

    // The helper range covers cols 1-4 (starts on continuation of first,
    // includes WIDE of third).
    {
        let target = row
            .cells_mut_with_fixup(1, 4)
            .expect("range inside row should produce a slice");
        assert_eq!(target.len(), 4);
    }

    // Col 0 should be cleared (orphaned first half when continuation at 1 is overwritten)
    assert_eq!(row.get(0).unwrap().char(), ' ');
    assert!(!row.get(0).unwrap().is_wide());

    // Col 5 should be cleared (orphaned continuation when WIDE at 4 is overwritten)
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
            row.cells_mut_with_fixup(u16::MAX, 1).is_none(),
            "out-of-bounds start should return None"
        );
    }));

    assert!(
        result.is_ok(),
        "out-of-bounds start should return early without overflow panic"
    );
    assert_eq!(row.len(), 0);
}

/// Regression test for #7669: `fixup_wide_boundary` must clear an orphaned
/// WIDE_CONTINUATION at `left` when `left-1` is NOT a WIDE cell.
#[test]
fn fixup_wide_boundary_clears_orphaned_continuation_at_left() {
    use crate::{CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Place a wide char at cols 2-3 so col 3 is WIDE_CONTINUATION.
    row.write_wide_char(2, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(2).unwrap().is_wide());
    assert!(row.get(3).unwrap().is_wide_continuation());

    // Overwrite col 2 (the WIDE half) with a normal char via get_mut,
    // leaving col 3 as an orphaned WIDE_CONTINUATION.
    *row.get_mut(2).unwrap() =
        crate::Cell::with_style_id('A', crate::StyleId::DEFAULT, CellFlags::empty());
    assert!(!row.get(2).unwrap().is_wide());
    assert!(row.get(3).unwrap().is_wide_continuation());

    // Call fixup_wide_boundary with left=3 (the orphaned continuation).
    // Before the fix for #7669, this was a no-op: the left boundary only
    // checked (prev_wide && !cur_cont), not (cur_cont && !prev_wide).
    row.fixup_wide_boundary(3, 6, 10, true);

    // The orphaned WIDE_CONTINUATION at col 3 should now be cleared.
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

/// Regression for #7522: `clear_range_with` (DECFRA/DECERA/BCE fill backing)
/// must clear an orphaned WIDE head just LEFT of the fill rect to `Cell::EMPTY`,
/// NOT paint it with the fill glyph/attributes. cells[start-1] is outside the
/// rect, so bleeding the fill one column left is a rendering bug.
#[test]
fn clear_range_with_does_not_bleed_fill_onto_left_wide_orphan() {
    use crate::{Cell, CellFlags, PackedColor, StyleId};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Wide char at cols 0-1 (col 0 WIDE, col 1 its continuation).
    row.write_wide_char(0, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(0).unwrap().is_wide());

    // A visible fill (distinct glyph) so we can tell EMPTY from fill.
    let fill = Cell::with_style_id('X', StyleId::DEFAULT, CellFlags::empty());
    assert!(
        !fill.is_empty(),
        "fill must be visible for this test to mean anything"
    );

    // Fill [1, 5): overwrites the continuation at col 1, orphaning the WIDE
    // head at col 0 (which sits OUTSIDE the rect).
    row.clear_range_with(1, 5, fill);

    // In-rect cells carry the fill.
    assert_eq!(
        row.get(3).unwrap().char(),
        'X',
        "in-rect cell should hold the fill"
    );
    // The orphaned WIDE head at col 0 must be cleared to EMPTY, not painted 'X'.
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

/// Regression for #7522, right boundary: an orphaned WIDE continuation just
/// RIGHT of the fill rect must be cleared to `Cell::EMPTY`, not painted with
/// the fill.
#[test]
fn clear_range_with_does_not_bleed_fill_onto_right_wide_orphan() {
    use crate::{Cell, CellFlags, PackedColor, StyleId};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Wide char at cols 6-7 (col 6 WIDE, col 7 its continuation).
    row.write_wide_char(6, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(7).unwrap().is_wide_continuation());

    let fill = Cell::with_style_id('X', StyleId::DEFAULT, CellFlags::empty());

    // Fill [2, 7): overwrites the WIDE head at col 6, orphaning the
    // continuation at col 7 (which sits OUTSIDE the rect).
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
    // The cleared orphan at col 7 was the row's last content cell (old_len == 8),
    // so the logical length must shrink to 7. A stale len of 8 would surface the
    // now-empty cell 7 as a trailing space via row_text / scrollback materialization.
    assert_eq!(
        row.len(),
        7,
        "len must not keep counting the cleared trailing wide orphan"
    );
}

/// Regression (round 12): DECLRMM-bounded insert that bisects a wide pair must
/// clear BOTH halves, so the continuation's bit-10 (WIDE_CONTINUATION, aliasing
/// PROTECTED) is not shifted right into a phantom protected spacer. Mirrors the
/// non-bounded insert_chars_fill. Found independently by an audit finder and a
/// sibling session.
#[test]
fn insert_chars_bounded_fill_clears_phantom_continuation() {
    use crate::{Cell, CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Wide char at cols 4-5.
    row.write_wide_char(4, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(4).unwrap().is_wide());
    assert!(row.get(5).unwrap().is_wide_continuation());

    // DECIC-style bounded insert of 1 cell at the continuation (col 5), within
    // margin [.., 9). This bisects the pair and shifts old cells[5] to cells[6].
    row.insert_chars_bounded_fill(5, 1, 9, Cell::EMPTY);

    assert!(
        !row.get(4).unwrap().is_wide(),
        "orphaned WIDE head at 4 must be cleared"
    );
    // cells[6] received the shifted continuation. It must NOT carry a stale bit-10:
    // its left neighbor (cell 5) is a fill, not WIDE, so a raw bit-10 there reads
    // as DECSCA-protected and would survive a later selective erase.
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

/// Regression (round 12): selective_erase_chars whose range starts on the
/// continuation of an UNprotected wide char must clear both halves. Clearing only
/// the head at col-1 leaves cells[col]'s bit-10 reading as PROTECTED (its head is
/// now a non-WIDE fill), so the erase loop would skip it — a phantom protected
/// orphan. Mirrors selective_clear_range / selective_wipe_range (bit-10 class).
#[test]
fn selective_erase_chars_clears_left_boundary_continuation() {
    use crate::{Cell, CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Unprotected wide char at cols 3-4.
    row.write_wide_char(3, '\u{4E2D}', fg, bg, CellFlags::empty());
    assert!(row.get(3).unwrap().is_wide());
    assert!(row.get(4).unwrap().is_wide_continuation());

    // Erase [4, 6): the range starts on the continuation; the head at 3 is outside.
    row.selective_erase_chars(4, 2, Cell::EMPTY);

    assert!(
        row.get(3).unwrap().is_empty(),
        "orphaned WIDE head at 3 must be erased"
    );
    assert!(
        !row.get(4)
            .unwrap()
            .flags()
            .contains(CellFlags::WIDE_CONTINUATION),
        "in-range continuation at 4 must be erased, not left phantom-protected"
    );
    assert!(
        !row.is_cell_protected(4),
        "cells[4] must not read as phantom-protected"
    );
    assert!(
        row.get(4).unwrap().is_empty(),
        "cells[4] should be a clean blank"
    );
}

/// Regression (round 13): clear_range_with with an EMPTY fill (EL/DECERA with
/// default SGR) that erases a trailing wide char must shrink len — the right
/// wide-orphan is cleared at index `end` (one past the range), and when it was
/// the tail (old_len == end + 1) the len-recalc must still fire.
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

    // Erase [0, 3): fill 0..3 empty; cells[2] is the WIDE head, so its
    // continuation at col 3 (== end) is the orphaned tail and gets cleared.
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

/// Regression (round 13): selective_clear_range (DECSEL/DECSED) whose WIDE-head
/// co-clear wipes the continuation at index `end` (one past the range) must
/// shrink len when that continuation was the tail content.
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

    // Range [0, 5): col 4 is an unprotected WIDE head, so its continuation at
    // col 5 (== end) is co-cleared. old_len (6) == end (5) + 1.
    row.selective_clear_range(0, 5);

    for c in 0..6 {
        assert!(row.get(c).unwrap().is_empty(), "cell {c} should be empty");
    }
    assert_eq!(
        row.len(),
        0,
        "len must shrink to 0 after erasing the trailing wide char"
    );
}

/// Regression (round 13): selective_wipe_range (DECSERA) shares the same
/// off-by-one; a default-colored wiped continuation at index `end` that was the
/// tail must shrink len.
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

/// Regression (round 14): clear_range (the 2-arg EMPTY sibling of
/// clear_range_with) had the same un-relaxed len guard; a trailing wide char
/// whose continuation is cleared at index `end` must still shrink len.
#[test]
fn clear_range_shrinks_len_on_tail_wide_orphan() {
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

    // clear_range(4, 5): cells[4] is the WIDE head, so its continuation at col 5
    // (== end) is the orphan cleared one past the range. old_len (6) == end (5) + 1.
    row.clear_range(4, 5);

    for c in 0..6 {
        assert!(row.get(c).unwrap().is_empty(), "cell {c} should be empty");
    }
    assert_eq!(
        row.len(),
        0,
        "len must shrink to 0 after erasing the trailing wide char"
    );
}

/// Regression (round 14): selective_erase_chars had the same un-relaxed len
/// guard as its siblings; the WIDE-head co-clear touches cells[end] (one past
/// the range), so when that was the tail content, len must still shrink.
#[test]
fn selective_erase_chars_shrinks_len_on_tail_wide_orphan() {
    use crate::{Cell, CellFlags, PackedColor};

    let (_pages, mut row) = make_row(10);
    for (i, ch) in ['a', 'b', 'c', 'd'].iter().enumerate() {
        row.write_char(i as u16, *ch);
    }
    row.write_wide_char(
        4,
        '\u{4E2D}',
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    assert_eq!(row.len(), 6, "abcd中 → len 6");

    // Erase just the WIDE head at col 4 (range [4,5)); its continuation at col 5
    // (== end) is co-cleared one past the range. Real content now ends at col 3.
    row.selective_erase_chars(4, 1, Cell::EMPTY);

    assert!(
        row.get(4).unwrap().is_empty(),
        "erased WIDE head must be empty"
    );
    assert!(
        row.get(5).unwrap().is_empty(),
        "co-cleared continuation must be empty"
    );
    assert_eq!(row.len(), 4, "len must shrink to 4 (content ends at 'd')");
    assert_eq!(
        row.get(3).unwrap().char(),
        'd',
        "surviving content must be intact"
    );
}

// ── Round 15: the #7522 len class on the WRITE/FILL paths ────────────────
//
// The clear/erase family (clear_range, selective_*, erase_chars) was hardened
// so a wide-orphan clear one column past the operation range tightens len when
// that orphan was the row's tail content. The write paths (write_char*,
// write_wide_char*, fill_cell_run) grew len monotonically and never recalced,
// so the identical off-by-one left len stale-high — a phantom trailing space in
// row_text/search/scrollback (all slice through self.len). These lock the fix.

/// write_char: a single-width write over a WIDE head at the tail clears the
/// continuation at col+1; when that pair was the row's last content
/// (old_len == col+2) len must tighten, not stay stale-high.
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

/// write_char_packed: same tail wide-head overwrite via the packed hot path.
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

/// write_wide_char_packed: a new wide char whose second half overwrites an
/// existing wide head orphans that char's continuation at col+2; when it was the
/// tail (old_len == col+3) len must tighten.
#[test]
fn write_wide_char_packed_shrinks_len_on_tail_wide_orphan() {
    use crate::{CellFlags, PackedColors};

    let (_pages, mut row) = make_row(4);
    row.write_wide_char_packed(2, '\u{4E00}', PackedColors::DEFAULT, CellFlags::empty());
    assert_eq!(row.len(), 4, "wide char at 2-3 → len 4");

    // New wide char at 1-2 overwrites the head at col 2; its continuation at col 3
    // (the tail) is orphaned and cleared.
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

/// fill_cell_run (REP/padding fast path): the fixup clears a wide continuation at
/// index `end` (one column past the fill); when that orphan was the tail content
/// (old_len == end+1) the grow-only len update misses it and len must tighten.
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

    // Fill [3, 6) with a non-empty template: the last filled cell (col 5) is the
    // WIDE head, so its continuation at col 6 (== end) is orphaned and cleared.
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

/// Regression (codex round-15 review): cells_mut_with_fixup is the bulk-write
/// primitive; its fixup clears a wide continuation one column PAST the returned
/// slice, but bulk callers (grid/write.rs, write_split.rs) only GROW len via
/// update_len(). When that orphan was the tail content, the primitive itself
/// must tighten len — the caller can't see the past-range clear.
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

    // Bulk ASCII write over [3, 6): the WIDE head at col 5 is covered, so its
    // continuation at col 6 (== end, one past the slice) is orphaned and cleared.
    {
        let target = row.cells_mut_with_fixup(3, 3).expect("range inside row");
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

/// Regression (codex round-15 review): write_wide_char_with_style_id must set
/// HAS_WIDE_CHARS so a later HAS_WIDE_CHARS-gated production fixup (write_char)
/// still runs — otherwise the orphaned continuation survives and len stays high.
#[test]
fn style_id_wide_write_sets_has_wide_chars_for_later_narrow_overwrite() {
    use crate::{CellFlags, RowFlags, StyleId};

    let (_pages, mut row) = make_row(10);
    row.write_wide_char_with_style_id(8, '\u{4E2D}', StyleId::new(3), CellFlags::empty());
    assert!(
        row.flags().contains(RowFlags::HAS_WIDE_CHARS),
        "wide write must set HAS_WIDE_CHARS"
    );
    assert_eq!(row.len(), 10, "wide char at 8-9 → len 10");

    // Production write_char gates its fixup on HAS_WIDE_CHARS; with the flag set it
    // now clears the continuation at col 9 and the recalc tightens len.
    row.write_char(8, 'X');
    assert!(
        row.get(9).unwrap().is_empty(),
        "continuation must be cleared by the now-enabled fixup"
    );
    assert_eq!(row.len(), 9, "len must tighten to 9");
}

/// Regression (round 16): delete_chars_fill (plain DCH, EMPTY fill) — the
/// right-boundary wide fixup clears the deleted WIDE head's orphaned continuation
/// at col+count; when that was the tail (old_len == col+count+1) the shift moves
/// the emptied cell into the tail, so `len = old_len - count` is stale-high by one.
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

    // DCH at col 2 deletes the WIDE head; its continuation at col 3 (the tail) is
    // cleared and shifted in. Real content is now just "ab".
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
