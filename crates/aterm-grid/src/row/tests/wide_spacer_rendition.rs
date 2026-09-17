// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates
//
// The wide-char continuation spacer inherits its lead's rendition.
//
// THE LAW: a rendition belongs to the CHARACTER, not to the column. A
// double-width character is one character in two columns, so the spacer must
// carry the lead's visual attributes and only its own role bit.
//
// MEASURED REASON (2026-09-16): spacers were built with `WIDE_CONTINUATION`
// alone. Under SGR 7 the renderer swapped fg/bg for the lead and not for the
// spacer, and since `lead.resolved_fg == spacer.resolved_bg` for EVERY colour
// pair, the glyph's spilled right half was composited invisibly — `\033[7m[漢字]`
// on a 9x17 cell gave 153 of 153 flat pixels per spacer, distinct=1, zero glyph
// ink. Underline/strikethrough/overline rules gapped under each right half too.

use super::super::*;
use super::make_row;
use crate::{CellFlags, PackedColor};

/// Every visual attribute bit, so a future SGR addition that lands in bits 0-13
/// is covered by construction rather than by a list someone has to remember to
/// extend.
const EVERY_RENDITION: CellFlags = CellFlags::from_bits(CellFlags::SPACER_RENDITION_MASK);

#[test]
fn wide_continuation_of_keeps_every_rendition_and_only_its_own_role_bit() {
    // A lead carrying every rendition bit plus both structural bits.
    let lead = EVERY_RENDITION
        .union(CellFlags::WIDE)
        .union(CellFlags::COMPLEX)
        .union(CellFlags::USES_STYLE_ID);
    let spacer = lead.wide_continuation_of();

    assert!(
        spacer.contains(EVERY_RENDITION),
        "the spacer must carry every rendition bit of its lead"
    );
    assert!(
        spacer.contains(CellFlags::WIDE_CONTINUATION),
        "the spacer keeps its own role bit"
    );
    assert!(
        !spacer.contains(CellFlags::WIDE),
        "the spacer is the tail of the pair, never a lead"
    );
    assert!(
        !spacer.contains(CellFlags::COMPLEX),
        "COMPLEX describes the spacer's OWN char_data (a space), not the lead's"
    );
    assert!(
        !spacer.contains(CellFlags::USES_STYLE_ID),
        "USES_STYLE_ID describes the spacer's OWN colors field; its constructor owns it"
    );
    assert_eq!(
        spacer.bits(),
        EVERY_RENDITION.bits() | CellFlags::WIDE_CONTINUATION.bits(),
        "rendition plus the role bit, and nothing else"
    );
}

#[test]
fn wide_continuation_of_a_bare_lead_is_the_bare_role_bit() {
    // Bit 10 aliases PROTECTED: a spacer must never gain DECSCA protection just
    // because its lead had it, and a plain lead must not gain anything at all.
    assert_eq!(
        CellFlags::empty().wide_continuation_of(),
        CellFlags::WIDE_CONTINUATION
    );
    assert_eq!(
        CellFlags::PROTECTED.wide_continuation_of(),
        CellFlags::WIDE_CONTINUATION,
        "PROTECTED aliases the role bit — the spacer carries exactly one bit 10"
    );
}

/// The primary CJK path: `Row::write_wide_char_packed` (what `[7m漢` takes).
#[test]
fn write_wide_char_inverse_reaches_the_spacer() {
    let (_pages, mut row) = make_row(10);
    row.write_wide_char(
        0,
        '\u{6F22}', // 漢
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::INVERSE,
    );

    let lead = row.get(0).unwrap();
    let spacer = row.get(1).unwrap();
    assert!(lead.is_wide() && lead.flags().contains(CellFlags::INVERSE));
    assert!(
        spacer.is_wide_continuation(),
        "the spacer keeps its role bit"
    );
    assert!(
        spacer.flags().contains(CellFlags::INVERSE),
        "the highlight band must be continuous across both halves of one character"
    );
    assert!(
        !spacer.flags().contains(CellFlags::WIDE),
        "the spacer must not become a second lead"
    );
}

/// The decorations that draw a RULE across the cell: a gap under the right half
/// is as visible as the lost glyph was.
#[test]
fn write_wide_char_rules_reach_the_spacer() {
    let (_pages, mut row) = make_row(10);
    let rules = CellFlags::UNDERLINE
        .union(CellFlags::STRIKETHROUGH)
        .union(CellFlags::OVERLINE)
        .union(CellFlags::BLINK);
    row.write_wide_char(
        0,
        '\u{5B57}', // 字
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        rules,
    );

    assert!(
        row.get(1).unwrap().flags().contains(rules),
        "underline, strikethrough, overline and blink must not gap at the column seam"
    );
}

/// The batch-run twin. A divergence here would highlight a wide run correctly
/// only when the writer happened to take the slow path.
#[test]
fn write_wide_char_no_fixup_matches_the_checked_path() {
    let (_pages, mut checked) = make_row(10);
    let (_pages2, mut fast) = make_row(10);
    let colors = Cell::convert_colors(PackedColor::DEFAULT_FG, PackedColor::DEFAULT_BG);

    checked.write_wide_char_packed(0, '\u{6F22}', colors, EVERY_RENDITION);
    // SAFETY: col 0 with col+1 = 1 < 10 cells, on a wide boundary, into a fresh
    // row with no pre-existing wide pair to conflict with (invariants a/b/c).
    unsafe { fast.write_wide_char_packed_no_fixup(0, '\u{6F22}', colors, EVERY_RENDITION) };

    assert_eq!(
        fast.get(1).unwrap().flags(),
        checked.get(1).unwrap().flags(),
        "the no-fixup batch path must build the same spacer as the checked path"
    );
    assert!(
        fast.get(1).unwrap().flags().contains(CellFlags::INVERSE),
        "and that spacer carries the lead's rendition"
    );
}

/// The StyleId twin of the same law.
#[test]
fn write_wide_char_with_style_id_inverse_reaches_the_spacer() {
    let (_pages, mut row) = make_row(10);
    row.write_wide_char_with_style_id(0, '\u{6F22}', crate::StyleId::new(3), CellFlags::INVERSE);

    let spacer = row.get(1).unwrap();
    assert!(spacer.is_wide_continuation(), "role bit kept");
    assert!(
        spacer.flags().contains(CellFlags::INVERSE),
        "the StyleId path obeys the same law as the packed-colour path"
    );
    assert!(
        spacer.flags().contains(CellFlags::USES_STYLE_ID),
        "with_style_id still owns the storage discriminant"
    );
    assert!(!spacer.flags().contains(CellFlags::WIDE));
}
