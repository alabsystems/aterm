// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for the grid cell module.

use super::*;

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
fn color_accessors_report_inline_colours_only() {
    let inline = Cell::with_style(
        'X',
        PackedColor::indexed(42),
        PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    assert_eq!(inline.fg_color(), Some(PackedColor::indexed(42)));
    assert_eq!(inline.bg_color(), Some(PackedColor::DEFAULT_BG));
    assert!(!inline.fg_needs_overflow() && !inline.bg_needs_overflow());

    let rgb = Cell::with_style(
        'X',
        PackedColor::rgb(255, 0, 0),
        PackedColor::rgb(0, 0, 255),
        CellFlags::empty(),
    );
    assert!(rgb.fg_needs_overflow() && rgb.bg_needs_overflow());
    assert_eq!((rgb.fg_color(), rgb.bg_color()), (None, None));
}

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

#[test]
fn has_extras_is_independent_of_complex_and_colours() {
    const HAS: u32 = 1 << 16;
    for low in 0..=u32::from(u16::MAX) {
        let (fg, bg) = (low as u8, (low >> 8) as u8);
        for top in [0x00u32, 0x01, 0x7F, 0x80, 0xFE, 0xFF] {
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
