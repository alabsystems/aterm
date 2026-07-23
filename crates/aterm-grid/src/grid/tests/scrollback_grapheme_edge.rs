// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Edge-case verification for `advance_grapheme_unit` (#5951 Prover verification).
//!
//! Migrated from aterm-core as part of #6556 Batch 2.

use super::super::*;

/// Unit test for `advance_grapheme_unit` with orphan ZWJ mid-text.
#[test]
fn advance_grapheme_unit_orphan_zwj_mid_text() {
    use crate::grid::scroll_materialize::advance_grapheme_unit;

    let text = "A\u{200D}B";

    let mut byte_idx = 0;
    let chars_consumed = advance_grapheme_unit(text, &mut byte_idx);
    assert_eq!(chars_consumed, 3, "orphan ZWJ should consume A + ZWJ + B");
    assert_eq!(byte_idx, text.len(), "should consume entire string");
    assert_eq!(&text[..byte_idx], "A\u{200D}B");
}

/// Unit test for consecutive combining marks.
#[test]
fn advance_grapheme_unit_consecutive_combining_marks() {
    use crate::grid::scroll_materialize::advance_grapheme_unit;

    let text = "e\u{0301}\u{0302}";

    let mut byte_idx = 0;
    let chars_consumed = advance_grapheme_unit(text, &mut byte_idx);
    assert_eq!(
        chars_consumed, 3,
        "consecutive combining marks should all join base char"
    );
    assert_eq!(byte_idx, text.len());
}

/// Variation selector after emoji: char + VS16 should be one grapheme unit.
#[test]
fn advance_grapheme_unit_variation_selector() {
    use crate::grid::scroll_materialize::advance_grapheme_unit;

    let text = "\u{2764}\u{FE0F}";

    let mut byte_idx = 0;
    let chars_consumed = advance_grapheme_unit(text, &mut byte_idx);
    assert_eq!(
        chars_consumed, 2,
        "variation selector should join base char"
    );
    assert_eq!(byte_idx, text.len());
}

/// Single ASCII char: advance_grapheme_unit should consume exactly 1 char.
#[test]
fn advance_grapheme_unit_single_ascii() {
    use crate::grid::scroll_materialize::advance_grapheme_unit;

    let text = "ABC";
    let mut byte_idx = 0;
    let chars_consumed = advance_grapheme_unit(text, &mut byte_idx);
    assert_eq!(
        chars_consumed, 1,
        "single ASCII should consume exactly 1 char"
    );
    assert_eq!(byte_idx, 1, "should advance exactly 1 byte for ASCII");
}

/// Effective width replays the live writer's VS16 widening: `❤` (U+2764,
/// text-presentation default, width 1) + VS16 must come back WIDE — the two
/// columns `widen_previous_cell_for_vs16` gave the live cell — so scrolled-back
/// rows do not shift the text after the heart by one column.
#[test]
fn advance_grapheme_unit_wide_vs16_widens_narrow_emoji() {
    use crate::grid::scroll_materialize::advance_grapheme_unit_wide;

    let text = "\u{2764}\u{FE0F}Z";
    let mut byte_idx = 0;
    let u = advance_grapheme_unit_wide(text, &mut byte_idx);
    let (chars, wide) = (u.chars, u.wide);
    assert_eq!(chars, 2, "heart + VS16 is one unit");
    assert!(wide, "VS16 widens the text-presentation heart to 2 cells");

    // A NON-emoji-capable base is not widened by a stray VS16.
    let text = "A\u{FE0F}";
    let mut byte_idx = 0;
    let u = advance_grapheme_unit_wide(text, &mut byte_idx);
    let (chars, wide) = (u.chars, u.wide);
    assert_eq!(chars, 2, "the selector still joins the unit");
    assert!(!wide, "VS16 must not widen a non-emoji-capable base");
}

/// Effective width replays the live writer's VS15 narrowing: `⌚` (U+231A,
/// emoji-presentation default, width 2) + VS15 must come back NARROW — matching
/// `narrow_previous_cell_for_vs15` — and a bare `⌚` stays wide.
#[test]
fn advance_grapheme_unit_wide_vs15_narrows_wide_emoji() {
    use crate::grid::scroll_materialize::advance_grapheme_unit_wide;

    let text = "\u{231A}\u{FE0E}Z";
    let mut byte_idx = 0;
    let u = advance_grapheme_unit_wide(text, &mut byte_idx);
    let (chars, wide) = (u.chars, u.wide);
    assert_eq!(chars, 2, "watch + VS15 is one unit");
    assert!(!wide, "VS15 narrows the emoji-presentation watch to 1 cell");

    let text = "\u{231A}Z";
    let mut byte_idx = 0;
    let u = advance_grapheme_unit_wide(text, &mut byte_idx);
    let (chars, wide) = (u.chars, u.wide);
    assert_eq!(chars, 1);
    assert!(wide, "a bare watch keeps its default 2-cell width");
}

/// The skin-tone fold gates on the EFFECTIVE width, mirroring the live
/// `try_combine_skin_tone_modifier` (which requires the previous CELL to be
/// wide): a text-presentation `☝🏽` stays SPLIT (the live modifier fell through
/// to its own wide cell), while the VS16-widened `☝️🏽` folds into one unit.
#[test]
fn advance_grapheme_unit_wide_skin_tone_fold_gates_on_effective_width() {
    use crate::grid::scroll_materialize::advance_grapheme_unit_wide;

    // Narrow base (no VS16): the modifier must START A NEW UNIT.
    let text = "\u{261D}\u{1F3FD}Z";
    let mut byte_idx = 0;
    let u = advance_grapheme_unit_wide(text, &mut byte_idx);
    let (chars, wide) = (u.chars, u.wide);
    assert_eq!(
        chars, 1,
        "narrow ☝ does not absorb the modifier (live split)"
    );
    assert!(!wide, "text-presentation ☝ is 1 cell");
    let u2 = advance_grapheme_unit_wide(text, &mut byte_idx);
    let (chars2, wide2) = (u2.chars, u2.wide);
    assert_eq!(chars2, 1, "the modifier is its own unit");
    assert!(
        wide2,
        "a standalone Fitzpatrick modifier renders as its own wide cell"
    );

    // VS16-widened base: the modifier folds, exactly like the live cell that
    // was widened before the modifier arrived.
    let text = "\u{261D}\u{FE0F}\u{1F3FD}Z";
    let mut byte_idx = 0;
    let u = advance_grapheme_unit_wide(text, &mut byte_idx);
    let (chars, wide) = (u.chars, u.wide);
    assert_eq!(chars, 3, "☝ + VS16 + modifier is one unit");
    assert!(wide, "the folded unit keeps the VS16-widened 2-cell width");
}

/// ZWJ emoji sequence: 👨‍👩‍👧 should be consumed as one grapheme unit.
#[test]
fn advance_grapheme_unit_zwj_emoji_sequence() {
    use crate::grid::scroll_materialize::advance_grapheme_unit;

    let text = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";

    let mut byte_idx = 0;
    let chars_consumed = advance_grapheme_unit(text, &mut byte_idx);
    assert_eq!(
        chars_consumed, 5,
        "ZWJ family emoji should consume all 5 codepoints"
    );
    assert_eq!(byte_idx, text.len());
}

/// A pathological endless ZWJ chain (as a crafted checkpoint / injected scrollback
/// `Line` can hold) must NOT be consumed into one unbounded grapheme unit: the
/// byte ceiling bounds `unit_str` so it cannot be allocated whole into a single
/// `complex_char` cell (memory-amplification DoS). The excess forms later units.
#[test]
fn advance_grapheme_unit_bounds_pathological_zwj_chain() {
    use crate::grid::scroll_materialize::advance_grapheme_unit;

    // 100k repetitions of "emoji + ZWJ" — ~800 KiB of one "grapheme" if unbounded.
    let unit = "\u{1F600}\u{200D}"; // 4-byte emoji + 3-byte ZWJ = 7 bytes
    let text = unit.repeat(100_000);

    let mut byte_idx = 0;
    let consumed = advance_grapheme_unit(&text, &mut byte_idx);
    // Bounded: the first unit stops at the ceiling (< 256 + one 4-byte char), not
    // the whole ~800 KiB string.
    assert!(
        byte_idx <= 256 + 4,
        "unit is bounded to the ceiling, got {byte_idx} bytes"
    );
    assert!(consumed > 0, "still makes forward progress");

    // The whole string is still consumable as a SEQUENCE of bounded units (forward
    // progress, no infinite loop, total bytes conserved).
    let mut idx = 0;
    let mut units = 0;
    while idx < text.len() {
        let before = idx;
        let n = advance_grapheme_unit(&text, &mut idx);
        assert!(idx > before, "each call advances byte_idx");
        assert!(n > 0 || idx > before);
        units += 1;
        assert!(units <= text.len(), "cannot loop more than byte count");
    }
    assert_eq!(
        idx,
        text.len(),
        "the full string is consumed across bounded units"
    );
}

/// Materialize round-trip for consecutive combining marks.
#[test]
fn materialize_consecutive_combining_marks() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let text = "e\u{0301}\u{0302}X";
    let attr_count = text.chars().count();
    let attrs: Rle<CellAttrs> = std::iter::repeat_n(CellAttrs::DEFAULT, attr_count).collect();
    let line = Line::with_attrs(text, attrs);

    let row = materialize_from_line(&line, 10);

    let extra = row.get_extra(0);
    assert!(
        extra.is_some(),
        "col 0 should have extras for double combining"
    );
    assert_eq!(
        extra.unwrap().complex_char().map(|s| &**s),
        Some("e\u{0301}\u{0302}"),
        "both combining marks should be preserved"
    );

    assert_eq!(row.cells[1].char(), 'X', "col 1 should be 'X'");
}

/// A wide char that cannot fit at the last column must STOP materialization, not
/// leave `col` stuck while the loop scans the rest of a (possibly injected,
/// over-long) line — an O(line length) spin under the lock. With cols=1 the leading
/// wide emoji cannot fit; without the col-progress break the following 'X' would be
/// placed at col 0 (and an oversized all-wide line would be scanned in full).
#[test]
fn materialize_stops_when_wide_char_cannot_fit_at_last_column() {
    use crate::grid::scroll_materialize::materialize_from_line;

    // Wide emoji (2 cols) then a narrow 'X', into a 1-column row.
    let text = "\u{1F600}X";
    let attr_count = text.chars().count();
    let attrs: Rle<CellAttrs> = std::iter::repeat_n(CellAttrs::DEFAULT, attr_count).collect();
    let line = Line::with_attrs(text, attrs);

    let row = materialize_from_line(&line, 1);
    assert_eq!(
        row.cells[0].char(),
        ' ',
        "the unfittable wide char is dropped and the scan stops — 'X' is NOT scanned in"
    );

    // A pathologically long all-wide line into a 1-col row must terminate and yield
    // a bounded (1-col) row rather than scanning every unit.
    let big = "\u{1F600}".repeat(200_000);
    let attrs2: Rle<CellAttrs> =
        std::iter::repeat_n(CellAttrs::DEFAULT, big.chars().count()).collect();
    let line2 = Line::with_attrs(&big, attrs2);
    let row2 = materialize_from_line(&line2, 1);
    assert_eq!(
        row2.cells.len(),
        1,
        "row is bounded to cols, not the line length"
    );
}

/// Materialize round-trip for orphan ZWJ between visible chars.
#[test]
fn materialize_orphan_zwj_between_visible_chars() {
    use crate::grid::scroll_materialize::materialize_from_line;

    let text = "A\u{200D}BX";
    let attr_count = text.chars().count();
    let attrs: Rle<CellAttrs> = std::iter::repeat_n(CellAttrs::DEFAULT, attr_count).collect();
    let line = Line::with_attrs(text, attrs);

    let row = materialize_from_line(&line, 10);

    let extra = row.get_extra(0);
    assert!(
        extra.is_some(),
        "col 0 should have extras for ZWJ-joined unit"
    );
    assert_eq!(
        extra.unwrap().complex_char().map(|s| &**s),
        Some("A\u{200D}B"),
        "ZWJ-joined unit should be preserved as complex_char"
    );

    assert_eq!(row.cells[1].char(), 'X', "col 1 should be 'X'");
}
