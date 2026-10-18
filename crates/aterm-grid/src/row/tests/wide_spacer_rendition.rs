// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates
//
// The wide-char continuation spacer inherits its lead's rendition: a
// double-width character is one character in two columns, so the spacer
// carries the lead's visual attributes plus only its own role bit. Otherwise
// SGR 7 paints the glyph's right half invisibly and underline, strikethrough
// and overline gap under it.

use super::super::*;
use super::make_row;
use crate::{CellFlags, PackedColor};

/// Every visual attribute bit, so a future SGR bit in 0-13 is covered
/// automatically.
const EVERY_RENDITION: CellFlags = CellFlags::from_bits(CellFlags::SPACER_RENDITION_MASK);

#[test]
fn wide_continuation_of_keeps_every_rendition_and_only_its_own_role_bit() {
    // A lead carrying every rendition bit plus both structural bits.
    let lead = EVERY_RENDITION
        .union(CellFlags::WIDE)
        .union(CellFlags::COMPLEX);
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
