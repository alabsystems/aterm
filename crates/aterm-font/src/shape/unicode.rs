// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/unicode.rs and src/hb/buffer.rs
// (`init_unicode_props`) and modified by the aterm project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! The Unicode properties the shaper keeps per character.
//!
//! The `Script` and `General_Category` tables are aterm-grapheme's (TF.6 of
//! docs/AUDIT-third-party-2026-10-15.md moved them there from the
//! `unicode-script` and `unicode-properties` crates). The predicates below
//! are the shaper's own versions of Unicode properties and stay with it:
//! HarfBuzz's modified `Default_Ignorable` (the Hangul fillers and the
//! shorthand format controls draw), its variation-selector ranges, and its
//! `Extended_Pictographic` list. The canonical combining class is not kept:
//! nothing that reads it (normalization, fallback mark positioning) is
//! carved.

use super::buffer::{
    GlyphInfo, SCRATCH_HAS_CGJ, SCRATCH_HAS_DEFAULT_IGNORABLES, SCRATCH_HAS_NON_ASCII,
    unicode_props,
};
use aterm_grapheme::GeneralCategory;

/// Default_Ignorable code points, as HarfBuzz hides them: U+115F, U+1160,
/// U+3164 and U+FFA0 are left out (fonts draw them as spacing glyphs, as
/// Uniscribe does), and so are U+1BCA0..1BCA3.
pub(crate) fn is_default_ignorable(c: char) -> bool {
    let ch = u32::from(c);
    let plane = ch >> 16;
    if plane == 0 {
        let page = ch >> 8;
        match page {
            0x00 => ch == 0x00AD,
            0x03 => ch == 0x034F,
            0x06 => ch == 0x061C,
            0x17 => (0x17B4..=0x17B5).contains(&ch),
            0x18 => (0x180B..=0x180E).contains(&ch),
            0x20 => {
                (0x200B..=0x200F).contains(&ch)
                    || (0x202A..=0x202E).contains(&ch)
                    || (0x2060..=0x206F).contains(&ch)
            }
            0xFE => (0xFE00..=0xFE0F).contains(&ch) || ch == 0xFEFF,
            0xFF => (0xFFF0..=0xFFF8).contains(&ch),
            _ => false,
        }
    } else {
        match plane {
            0x01 => (0x1D173..=0x1D17A).contains(&ch),
            0x0E => (0xE0000..=0xE0FFF).contains(&ch),
            _ => false,
        }
    }
}

/// VARIATION SELECTOR-1..16 and -17..256. The Mongolian free variation
/// selectors are the Arabic shaper's.
pub(crate) fn is_variation_selector(c: char) -> bool {
    let ch = u32::from(c);
    (0xFE00..=0xFE0F).contains(&ch) || (0xE0100..=0xE01EF).contains(&ch)
}

/// `Extended_Pictographic`, as upstream's generated list has it.
pub(crate) fn is_emoji_extended_pictographic(c: char) -> bool {
    matches!(
        u32::from(c),
        0x00A9
            | 0x00AE
            | 0x203C
            | 0x2049
            | 0x2122
            | 0x2139
            | 0x2194..=0x2199
            | 0x21A9..=0x21AA
            | 0x231A..=0x231B
            | 0x2328
            | 0x2388
            | 0x23CF
            | 0x23E9..=0x23F3
            | 0x23F8..=0x23FA
            | 0x24C2
            | 0x25AA..=0x25AB
            | 0x25B6
            | 0x25C0
            | 0x25FB..=0x25FE
            | 0x2600..=0x2605
            | 0x2607..=0x2612
            | 0x2614..=0x2685
            | 0x2690..=0x2705
            | 0x2708..=0x2712
            | 0x2714
            | 0x2716
            | 0x271D
            | 0x2721
            | 0x2728
            | 0x2733..=0x2734
            | 0x2744
            | 0x2747
            | 0x274C
            | 0x274E
            | 0x2753..=0x2755
            | 0x2757
            | 0x2763..=0x2767
            | 0x2795..=0x2797
            | 0x27A1
            | 0x27B0
            | 0x27BF
            | 0x2934..=0x2935
            | 0x2B05..=0x2B07
            | 0x2B1B..=0x2B1C
            | 0x2B50
            | 0x2B55
            | 0x3030
            | 0x303D
            | 0x3297
            | 0x3299
            | 0x1F000..=0x1F0FF
            | 0x1F10D..=0x1F10F
            | 0x1F12F
            | 0x1F16C..=0x1F171
            | 0x1F17E..=0x1F17F
            | 0x1F18E
            | 0x1F191..=0x1F19A
            | 0x1F1AD..=0x1F1E5
            | 0x1F201..=0x1F20F
            | 0x1F21A
            | 0x1F22F
            | 0x1F232..=0x1F23A
            | 0x1F23C..=0x1F23F
            | 0x1F249..=0x1F3FA
            | 0x1F400..=0x1F53D
            | 0x1F546..=0x1F64F
            | 0x1F680..=0x1F6FF
            | 0x1F774..=0x1F77F
            | 0x1F7D5..=0x1F7FF
            | 0x1F80C..=0x1F80F
            | 0x1F848..=0x1F84F
            | 0x1F85A..=0x1F85F
            | 0x1F888..=0x1F88F
            | 0x1F8AE..=0x1F8FF
            | 0x1F90C..=0x1F93A
            | 0x1F93C..=0x1F945
            | 0x1F947..=0x1FAFF
            | 0x1FC00..=0x1FFFD
    )
}

/// Sets a character's general category and Unicode property bits, and the
/// buffer's scratch flags they imply.
pub(crate) fn init_unicode_props(info: &mut GlyphInfo, scratch_flags: &mut u32) {
    let u = info.as_char();
    let gc = aterm_grapheme::general_category(u);
    info.gc = gc;
    let mut props = 0u16;
    if u32::from(u) >= 0x80 {
        *scratch_flags |= SCRATCH_HAS_NON_ASCII;
        if is_default_ignorable(u) {
            props |= unicode_props::IGNORABLE;
            *scratch_flags |= SCRATCH_HAS_DEFAULT_IGNORABLES;
            match u32::from(u) {
                0x200C => props |= unicode_props::CF_ZWNJ,
                0x200D => props |= unicode_props::CF_ZWJ,
                // Mongolian free variation selectors are hidden like other
                // default ignorables but take part in shaping (harfbuzz#234),
                // and so do TAG characters (harfbuzz#463).
                0x180B..=0x180D | 0x180F | 0xE0020..=0xE007F => props |= unicode_props::HIDDEN,
                // COMBINING GRAPHEME JOINER is not skipped in GSUB (harfbuzz#554).
                0x034F => {
                    props |= unicode_props::HIDDEN;
                    *scratch_flags |= SCRATCH_HAS_CGJ;
                }
                _ => {}
            }
        }
        if gc.is_mark() {
            props |= unicode_props::CONTINUATION;
        }
    }
    info.unicode_props = props;
}

#[inline]
pub(crate) fn is_unicode_mark(info: &GlyphInfo) -> bool {
    info.gc.is_mark()
}

/// Sets the general category, clearing the property byte that depends on it
/// (the `Cf` joiner and variation-selector bits), as upstream's packed form
/// does.
#[inline]
pub(crate) fn set_general_category(info: &mut GlyphInfo, gc: GeneralCategory) {
    info.gc = gc;
    info.unicode_props &= 0x00FF;
}

/// Marks a variation selector the font could not map with its base, so GSUB
/// sees it as a format character (`true`), or restores it (`false`).
#[inline]
pub(crate) fn set_variation_selector(info: &mut GlyphInfo, customize: bool) {
    if customize {
        set_general_category(info, GeneralCategory::Format);
        info.unicode_props |= unicode_props::CF_VS;
    } else {
        set_general_category(info, GeneralCategory::NonspacingMark);
    }
}

#[inline]
pub(crate) fn is_default_ignorable_info(info: &GlyphInfo) -> bool {
    info.unicode_props & unicode_props::IGNORABLE != 0 && !super::layout::substituted(info)
}

#[inline]
pub(crate) fn is_hidden(info: &GlyphInfo) -> bool {
    info.unicode_props & unicode_props::HIDDEN != 0
}

#[inline]
pub(crate) fn unhide(info: &mut GlyphInfo) {
    info.unicode_props &= !unicode_props::HIDDEN;
}

#[inline]
pub(crate) fn set_continuation(info: &mut GlyphInfo) {
    info.unicode_props |= unicode_props::CONTINUATION;
}

#[inline]
pub(crate) fn is_continuation(info: &GlyphInfo) -> bool {
    info.unicode_props & unicode_props::CONTINUATION != 0
}

#[inline]
pub(crate) fn is_zwnj(info: &GlyphInfo) -> bool {
    info.gc == GeneralCategory::Format && info.unicode_props & unicode_props::CF_ZWNJ != 0
}

#[inline]
pub(crate) fn is_zwj(info: &GlyphInfo) -> bool {
    info.gc == GeneralCategory::Format && info.unicode_props & unicode_props::CF_ZWJ != 0
}
