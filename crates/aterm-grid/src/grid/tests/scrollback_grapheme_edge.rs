// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Edge cases for `advance_grapheme_unit`.

use super::super::*;

/// Printable ASCII after an orphan ZWJ opens a unit of its own, as the live grid draws `A` ZWJ `B`
/// in two cells (`print_ascii_bulk` never folds); a non-ASCII char after it still folds.
#[test]
fn advance_grapheme_unit_orphan_zwj_mid_text() {
    use crate::grid::scroll_materialize::advance_grapheme_unit;

    let text = "A\u{200D}B";

    let mut byte_idx = 0;
    let chars_consumed = advance_grapheme_unit(text, &mut byte_idx);
    assert_eq!(chars_consumed, 2, "the ZWJ joins A, and ASCII B does not");
    assert_eq!(&text[..byte_idx], "A\u{200D}");
    assert_eq!(advance_grapheme_unit(text, &mut byte_idx), 1);
    assert_eq!(byte_idx, text.len(), "B is a unit of its own");

    let text = "A\u{200D}\u{e9}";
    let mut byte_idx = 0;
    assert_eq!(
        advance_grapheme_unit(text, &mut byte_idx),
        3,
        "a non-ASCII char after the ZWJ folds into the unit"
    );
    assert_eq!(byte_idx, text.len());
}

/// A regional indicator folds onto a unit whose base is a lone one, past a mark between them, as
/// the live grid folds it (`try_combine_regional_indicator` reads the cell's base) and as
/// `aterm_grapheme::GridColumns` lays a line out: `🇺́🇸` is one unit. A flag's third indicator
/// starts a unit, and so does one after an indicator a ZWJ joined to another base.
#[test]
fn advance_grapheme_unit_folds_a_flag_half_onto_its_base_past_a_mark() {
    use crate::grid::scroll_materialize::advance_grapheme_unit;

    let units = |text: &'static str| {
        let mut byte_idx = 0;
        let mut out = Vec::new();
        while byte_idx < text.len() {
            let start = byte_idx;
            advance_grapheme_unit(text, &mut byte_idx);
            out.push(&text[start..byte_idx]);
        }
        out
    };
    for (text, want) in [
        ("\u{1F1FA}\u{1F1F8}", vec!["\u{1F1FA}\u{1F1F8}"]),
        (
            "\u{1F1FA}\u{301}\u{1F1F8}",
            vec!["\u{1F1FA}\u{301}\u{1F1F8}"],
        ),
        (
            "\u{1F1FA}\u{1F1F8}\u{1F1EB}",
            vec!["\u{1F1FA}\u{1F1F8}", "\u{1F1EB}"],
        ),
        (
            "\u{1F1FA}\u{301}\u{1F1F8}\u{1F1EB}",
            vec!["\u{1F1FA}\u{301}\u{1F1F8}", "\u{1F1EB}"],
        ),
        (
            "\u{e9}\u{200D}\u{1F1FA}\u{1F1F8}",
            vec!["\u{e9}\u{200D}\u{1F1FA}", "\u{1F1F8}"],
        ),
    ] {
        assert_eq!(units(text), want, "{text:?}");
    }
}

/// Consecutive combining marks join the base.
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

/// An emoji plus VS16 is one unit.
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

/// A single ASCII char is one unit of one byte.
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

/// Effective width replays the live writer's VS16 widening: `❤` + VS16 comes
/// back wide, so text after the heart does not shift.
#[test]
fn advance_grapheme_unit_wide_vs16_widens_narrow_emoji() {
    use crate::grid::scroll_materialize::advance_grapheme_unit_wide;

    let text = "\u{2764}\u{FE0F}Z";
    let mut byte_idx = 0;
    let u = advance_grapheme_unit_wide(text, &mut byte_idx);
    let (chars, wide) = (u.chars, u.wide);
    assert_eq!(chars, 2, "heart + VS16 is one unit");
    assert!(wide, "VS16 widens the text-presentation heart to 2 cells");

    // A stray VS16 does not widen a non-emoji-capable base.
    let text = "A\u{FE0F}";
    let mut byte_idx = 0;
    let u = advance_grapheme_unit_wide(text, &mut byte_idx);
    let (chars, wide) = (u.chars, u.wide);
    assert_eq!(chars, 2, "the selector still joins the unit");
    assert!(!wide, "VS16 must not widen a non-emoji-capable base");
}

/// Effective width replays VS15 narrowing: `⌚` + VS15 comes back narrow, and a
/// bare `⌚` stays wide.
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

/// The skin-tone fold gates on effective width, as the live
/// `try_combine_skin_tone_modifier` does: text-presentation `☝🏽` stays split,
/// VS16-widened `☝️🏽` folds into one unit.
#[test]
fn advance_grapheme_unit_wide_skin_tone_fold_gates_on_effective_width() {
    use crate::grid::scroll_materialize::advance_grapheme_unit_wide;

    // Narrow base (no VS16): the modifier starts a new unit.
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

    // VS16-widened base: the modifier folds, as on a live cell widened before it arrived.
    let text = "\u{261D}\u{FE0F}\u{1F3FD}Z";
    let mut byte_idx = 0;
    let u = advance_grapheme_unit_wide(text, &mut byte_idx);
    let (chars, wide) = (u.chars, u.wide);
    assert_eq!(chars, 3, "☝ + VS16 + modifier is one unit");
    assert!(wide, "the folded unit keeps the VS16-widened 2-cell width");
}

/// A ZWJ family emoji is one unit.
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

/// An endless ZWJ chain (from a crafted checkpoint or scrollback `Line`) is not
/// consumed into one unit: a byte ceiling bounds `unit_str`, and the excess
/// forms later units.
#[test]
fn advance_grapheme_unit_bounds_pathological_zwj_chain() {
    use crate::grid::scroll_materialize::advance_grapheme_unit;

    // 700 KB of one "grapheme" if unbounded.
    let unit = "\u{1F600}\u{200D}"; // 4-byte emoji + 3-byte ZWJ = 7 bytes
    let text = unit.repeat(100_000);

    let mut byte_idx = 0;
    let consumed = advance_grapheme_unit(&text, &mut byte_idx);
    // The first unit stops at the ceiling (< 256 bytes plus one 4-byte char).
    assert!(
        byte_idx <= 256 + 4,
        "unit is bounded to the ceiling, got {byte_idx} bytes"
    );
    assert!(consumed > 0, "still makes forward progress");

    // The whole string is consumed as a sequence of bounded units.
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

/// Consecutive combining marks materialize into one cell.
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

/// A wide char that cannot fit at the last column stops materialization rather than scanning the
/// rest of the line with `col` stuck.
#[test]
fn materialize_stops_when_wide_char_cannot_fit_at_last_column() {
    use crate::grid::scroll_materialize::materialize_from_line;

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

    // A pathologically long all-wide line still yields a 1-col row.
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

/// An orphan ZWJ between visible chars stays in `A`'s cell and `B` gets its own, as the live grid
/// draws them.
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
        Some("A\u{200D}"),
        "the ZWJ stays in A's cell"
    );

    assert_eq!(row.cells[1].char(), 'B', "col 1 should be 'B'");
    assert_eq!(row.cells[2].char(), 'X', "col 2 should be 'X'");
}
