// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for the grid cell module.

use super::*;

#[test]
fn cell_new_bmp() {
    let cell = Cell::new('A');
    assert_eq!(cell.char_data(), 'A' as u16);
    assert!(!cell.is_complex());
    assert_eq!(cell.char(), 'A');
    assert_eq!(cell.codepoint(), 'A' as u32);
}

#[test]
fn cell_new_non_bmp() {
    // Emoji (non-BMP) should trigger complex handling
    let cell = Cell::new('\u{1F600}');
    // Non-BMP can't be stored directly in 16 bits
    // Cell::new stores replacement char for non-BMP
    assert_eq!(cell.char(), '\u{FFFD}');
}

#[test]
fn cell_cjk() {
    // CJK characters are in BMP
    let cell = Cell::new('\u{3042}');
    assert_eq!(cell.char_data(), '\u{3042}' as u16);
    assert!(!cell.is_complex());
    assert_eq!(cell.char(), '\u{3042}');
}

#[test]
fn cell_pack_unpack_flags() {
    let flags = CellFlags::BOLD.union(CellFlags::ITALIC);
    let cell = Cell::with_style('X', PackedColor::DEFAULT_FG, PackedColor::DEFAULT_BG, flags);
    assert!(cell.flags().contains(CellFlags::BOLD));
    assert!(cell.flags().contains(CellFlags::ITALIC));
    assert!(!cell.flags().contains(CellFlags::UNDERLINE));
}

#[test]
fn cell_is_empty() {
    assert!(Cell::EMPTY.is_empty());
    assert!(Cell::default().is_empty());

    let cell = Cell::new('X');
    assert!(!cell.is_empty());
}

#[test]
fn cell_set_methods() {
    let mut cell = Cell::EMPTY;

    cell.set_char('Z');
    assert_eq!(cell.char(), 'Z');

    cell.set_fg(PackedColor::indexed(100));
    assert!(cell.colors().fg_is_indexed());
    assert_eq!(cell.colors().fg_index(), 100);

    cell.set_flags(CellFlags::STRIKETHROUGH);
    assert!(cell.flags().contains(CellFlags::STRIKETHROUGH));
}

#[test]
fn set_char_bmp_stores_codepoint() {
    let mut cell = Cell::EMPTY;

    cell.set_char('A');
    assert_eq!(cell.char(), 'A');
    assert_eq!(cell.char_data(), 'A' as u16);
    assert!(!cell.is_complex());

    // CJK (BMP)
    cell.set_char('\u{4E16}'); // 世
    assert_eq!(cell.char(), '\u{4E16}');
    assert_eq!(cell.char_data(), 0x4E16);

    // Max BMP codepoint
    cell.set_char('\u{FFFF}');
    assert_eq!(cell.char_data(), 0xFFFF);
}

#[test]
fn set_char_clears_complex_flag() {
    let mut cell = Cell::with_overflow_index(42);
    assert!(cell.is_complex());

    cell.set_char('X');
    assert!(!cell.is_complex());
    assert_eq!(cell.char(), 'X');
}

#[test]
fn new_non_bmp_stores_replacement() {
    // Cell::new() stores U+FFFD for non-BMP characters rather than dropping
    // them (set_char() does the same in release; debug builds assert).
    let emoji_cell = Cell::new('\u{1F600}'); // 😀
    assert_eq!(emoji_cell.char(), '\u{FFFD}');
    assert_eq!(emoji_cell.char_data(), '\u{FFFD}' as u16);
    assert!(!emoji_cell.is_complex());

    // CJK Extension B (non-BMP)
    let cjk_ext = Cell::new('\u{20000}');
    assert_eq!(cjk_ext.char(), '\u{FFFD}');

    // Mathematical symbol (non-BMP)
    let math = Cell::new('\u{1D400}'); // 𝐀
    assert_eq!(math.char(), '\u{FFFD}');
}

#[test]
fn set_char_constructor_parity() {
    // Cell::new() and Cell::set_char() should produce identical results for BMP
    let via_new = Cell::new('Z');
    let mut via_set = Cell::EMPTY;
    via_set.set_char('Z');
    assert_eq!(via_new.char(), via_set.char());
    assert_eq!(via_new.char_data(), via_set.char_data());
    assert_eq!(via_new.is_complex(), via_set.is_complex());
}

#[test]
fn cell_with_overflow_index() {
    let cell = Cell::with_overflow_index(42);
    assert!(cell.is_complex());
    assert_eq!(cell.char_data(), 42);
    assert_eq!(cell.codepoint(), 0xFFFD); // Returns replacement for complex
    assert_eq!(cell.char(), '\u{FFFD}');
}

#[test]
fn cell_rgb_needs_overflow() {
    let cell = Cell::with_style(
        'X',
        PackedColor::rgb(255, 0, 0),
        PackedColor::rgb(0, 0, 255),
        CellFlags::empty(),
    );
    assert!(cell.fg_needs_overflow());
    assert!(cell.bg_needs_overflow());
    assert_eq!(cell.fg_color(), None);
    assert_eq!(cell.bg_color(), None);
}

#[test]
fn cell_inline_color_accessors_return_inline_colors() {
    let cell = Cell::with_style(
        'X',
        PackedColor::indexed(42),
        PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );

    assert_eq!(cell.fg_color(), Some(PackedColor::indexed(42)));
    assert_eq!(cell.bg_color(), Some(PackedColor::DEFAULT_BG));
}

#[test]
fn cell_style_id_color_accessors_return_none() {
    let cell = Cell::with_style_id('S', StyleId::new(42), CellFlags::empty());

    assert!(cell.uses_style_id());
    assert_eq!(cell.fg_color(), None);
    assert_eq!(cell.bg_color(), None);
    assert!(!cell.fg_needs_overflow());
    assert!(!cell.bg_needs_overflow());
}

// StyleId tests

#[test]
fn cell_flags_uses_style_id() {
    let flags = CellFlags::USES_STYLE_ID;
    assert!(flags.uses_style_id());
    assert!(!flags.is_complex());

    let combined = CellFlags::USES_STYLE_ID.union(CellFlags::WIDE);
    assert!(combined.uses_style_id());
    assert!(combined.contains(CellFlags::WIDE));
}

// HAS_EXTRAS flag tests

#[test]
fn cell_has_extras_default_false() {
    assert!(!Cell::EMPTY.has_extras());
    assert!(!Cell::new('A').has_extras());
}

#[test]
fn cell_set_has_extras_roundtrip() {
    let mut cell = Cell::new('A');
    assert!(!cell.has_extras());

    cell.set_has_extras(true);
    assert!(cell.has_extras());
    assert_eq!(cell.char(), 'A');

    cell.set_has_extras(false);
    assert!(!cell.has_extras());
}

#[test]
fn cell_has_extras_preserved_with_colors() {
    let mut cell = Cell::with_style(
        'X',
        PackedColor::indexed(196),
        PackedColor::indexed(21),
        CellFlags::BOLD,
    );
    assert!(!cell.has_extras());

    cell.set_has_extras(true);
    assert!(cell.has_extras());
    assert!(cell.colors().fg_is_indexed());
    assert_eq!(cell.colors().fg_index(), 196);
    assert!(cell.flags().contains(CellFlags::BOLD));
}

#[test]
fn cell_from_ascii_styled_with_extras_flag() {
    let colors = PackedColors::with_indexed(196, 21).with_extras_flag();
    let cell = Cell::from_ascii_styled(b'X', colors, CellFlags::empty());
    assert!(cell.has_extras());
    assert_eq!(cell.char(), 'X');
}

// Cell memory layout: the three fields sit at fixed bit offsets

/// THE CELL PACKING IS LOSSLESS OVER EVERY INPUT. Each domain is small enough
/// to enumerate whole:
/// - every `char`: `Cell::new` stores a BMP char directly and anything past
///   the BMP as U+FFFD, and is never complex; `set_char` on a complex cell
///   clears COMPLEX and stores the BMP char exactly as `Cell::new` does, and
///   on `Cell::EMPTY` it agrees with `Cell::new` (constructor parity);
/// - every visual flag set (`u16 & VISUAL_FLAGS_MASK`) round-trips;
/// - every (fg, bg) index pair round-trips through `PackedColors::with_indexed`;
/// - every overflow index: `with_overflow_index` is complex, carries the index
///   and reads U+FFFD as its codepoint.
#[test]
fn cell_packing_is_lossless_over_every_input() {
    for c in (0..=0x10_FFFF).filter_map(char::from_u32) {
        let cell = Cell::new(c);
        assert!(!cell.is_complex(), "{c:?}");
        let cp = u32::from(c);
        if cp <= Cell::MAX_DIRECT_CODEPOINT {
            assert_eq!(u32::from(cell.char_data()), cp, "{c:?}");
            assert_eq!(cell.char(), c);
            assert_eq!(cell.codepoint(), cp);
            for index in [0, 7, u16::MAX] {
                let mut complex = Cell::with_overflow_index(index);
                complex.set_char(c);
                assert!(!complex.is_complex(), "{c:?} over index {index}");
                assert_eq!(complex.char_data(), cell.char_data(), "{c:?}");
                assert_eq!(complex.char(), c);
            }
            let mut via_set = Cell::EMPTY;
            via_set.set_char(c);
            assert_eq!(via_set.char_data(), cell.char_data(), "{c:?}");
            assert_eq!(via_set.is_complex(), cell.is_complex(), "{c:?}");
        } else {
            assert_eq!(cell.char_data(), 0xFFFD, "{c:?}");
            assert_eq!(cell.char(), '\u{FFFD}');
        }
    }
    for bits in 0..=u16::MAX {
        let visual = bits & CellFlags::VISUAL_FLAGS_MASK;
        let cell = Cell::with_style(
            ' ',
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::from_bits(visual),
        );
        assert_eq!(cell.flags().bits() & CellFlags::VISUAL_FLAGS_MASK, visual);
    }
    for fg in 0..=u8::MAX {
        for bg in 0..=u8::MAX {
            let colors = PackedColors::with_indexed(fg, bg);
            assert!(colors.fg_is_indexed() && colors.bg_is_indexed());
            assert_eq!((colors.fg_index(), colors.bg_index()), (fg, bg));
        }
    }
    for index in 0..=u16::MAX {
        let cell = Cell::with_overflow_index(index);
        assert!(cell.is_complex());
        assert_eq!(cell.char_data(), index);
        assert_eq!(cell.codepoint(), 0xFFFD);
    }
}

/// HAS_EXTRAS (PackedColors bit 16) IS ORTHOGONAL TO EVERYTHING ELSE A CELL
/// HOLDS, which the render path's `has_extras() || is_complex()` probe and the
/// StyleId encoding both rely on:
/// - the flag operations touch bit 16 and nothing else, and every colour
///   setter leaves it alone, for every fg/bg index and every fg/bg mode
///   nibble, with the unused bits 17..=23 clear and set;
/// - HAS_EXTRAS and COMPLEX set and clear independently for every overflow
///   index, and `set_has_extras` round-trips a cell's colours;
/// - a StyleId never sets it (`with_style_id`, `from_ascii_with_style_id`,
///   every `u16`), and `Cell::EMPTY` — what erases write — has it clear.
#[test]
fn has_extras_is_independent_of_complex_colours_and_style_ids() {
    const HAS: u32 = 1 << 16;
    for low in 0..=u32::from(u16::MAX) {
        let (fg, bg) = (low as u8, (low >> 8) as u8);
        for top in 0..=0xFFu32 {
            for spare in [0, 0x7F << 17] {
                for raw in [top << 24 | spare | low, top << 24 | spare | HAS | low] {
                    let colors = PackedColors(raw);
                    assert_eq!(colors.has_extras(), raw & HAS != 0, "{raw:#010x}");
                    assert_eq!(colors.with_extras_flag().0, raw | HAS, "{raw:#010x}");
                    assert_eq!(colors.without_extras_flag().0, raw & !HAS, "{raw:#010x}");
                    for set in [
                        colors.set_fg_indexed(fg),
                        colors.set_bg_indexed(bg),
                        colors.with_rgb_fg(),
                        colors.with_rgb_bg(),
                        colors.set_fg_default(),
                        colors.set_bg_default(),
                    ] {
                        assert_eq!(set.0 & HAS, raw & HAS, "{raw:#010x}");
                    }
                    assert_eq!(colors.set_fg_indexed(fg).fg_index(), fg);
                    assert_eq!(colors.set_bg_indexed(bg).bg_index(), bg);
                }
            }
        }
    }
    for index in 0..=u16::MAX {
        let mut complex = Cell::with_overflow_index(index);
        assert!(!complex.has_extras());
        for has in [true, false] {
            complex.set_has_extras(has);
            assert_eq!(complex.has_extras(), has);
            assert!(complex.is_complex() && complex.char_data() == index);
        }
        let mut plain = Cell::EMPTY;
        plain.set_has_extras(true);
        plain.set_overflow_index(index);
        assert!(plain.is_complex() && plain.has_extras());
        assert_eq!(plain.char_data(), index);

        let style_id = super::super::style::StyleId::new(index);
        assert!(!Cell::with_style_id('A', style_id, CellFlags::empty()).has_extras());
        assert!(!Cell::from_ascii_with_style_id(b'A', style_id, CellFlags::empty()).has_extras());
    }
    for fg in 0..=u8::MAX {
        for bg in 0..=u8::MAX {
            let colors = PackedColors::with_indexed(fg, bg);
            let mut cell = Cell::from_ascii_styled(b'A', colors, CellFlags::empty());
            cell.set_has_extras(true);
            assert_eq!(cell.colors(), colors.with_extras_flag());
            cell.set_has_extras(false);
            assert_eq!(cell.colors(), colors);
        }
    }
    assert!(!Cell::EMPTY.has_extras() && Cell::EMPTY.is_empty());
}
