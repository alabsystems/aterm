// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for the grid cell module.
//!
//! Extracted from cell.rs (#1977).

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
fn cell_clear() {
    let mut cell = Cell::with_style(
        'X',
        PackedColor::indexed(196),
        PackedColor::indexed(21),
        CellFlags::BOLD,
    );
    cell.clear();
    assert!(cell.is_empty());
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

    // ASCII
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
    // Cell::new() with non-BMP characters stores U+FFFD (replacement char)
    // instead of silently dropping. This documents the API contract that
    // set_char() also follows in release mode (debug builds catch misuse
    // via debug_assert).
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

// =========================================================================
// StyleId tests
// =========================================================================

#[test]
fn cell_with_style_id_default() {
    use super::super::style::StyleId;
    let cell = Cell::with_style_id('A', StyleId::DEFAULT, CellFlags::empty());
    assert!(cell.uses_style_id());
    assert_eq!(cell.style_id(), StyleId::DEFAULT);
    assert_eq!(cell.char(), 'A');
    assert!(!cell.is_complex());
}

#[test]
fn cell_with_style_id_non_default() {
    let style_id = StyleId::new(42);
    let cell = Cell::with_style_id('X', style_id, CellFlags::empty());
    assert!(cell.uses_style_id());
    assert_eq!(cell.style_id(), style_id);
    assert_eq!(cell.char(), 'X');
}

#[test]
fn cell_with_style_id_preserves_cell_flags() {
    let style_id = StyleId::new(100);
    let cell = Cell::with_style_id('W', style_id, CellFlags::WIDE);
    assert!(cell.uses_style_id());
    assert!(cell.flags().contains(CellFlags::WIDE));
    assert!(cell.flags().contains(CellFlags::USES_STYLE_ID));
    assert_eq!(cell.style_id(), style_id);
}

#[test]
fn cell_from_ascii_with_style_id() {
    let style_id = StyleId::new(5);
    let cell = Cell::from_ascii_with_style_id(b'H', style_id, CellFlags::empty());
    assert!(cell.uses_style_id());
    assert_eq!(cell.style_id(), style_id);
    assert_eq!(cell.char(), 'H');
    assert_eq!(cell.fg_color(), None);
    assert_eq!(cell.bg_color(), None);
    assert!(!cell.fg_needs_overflow());
    assert!(!cell.bg_needs_overflow());
}

#[test]
fn cell_style_id_opt_when_using_style() {
    let style_id = StyleId::new(77);
    let cell = Cell::with_style_id('Y', style_id, CellFlags::empty());
    assert_eq!(cell.style_id_opt(), Some(style_id));
}

#[test]
fn cell_style_id_opt_when_using_inline_colors() {
    let cell = Cell::new('Z');
    assert!(!cell.uses_style_id());
    assert_eq!(cell.style_id_opt(), None);
}

#[test]
fn cell_set_style_id() {
    let mut cell = Cell::new('A');
    assert!(!cell.uses_style_id());

    let style_id = StyleId::new(123);
    cell.set_style_id(style_id);

    assert!(cell.uses_style_id());
    assert_eq!(cell.style_id(), style_id);
    // Character should be preserved
    assert_eq!(cell.char(), 'A');
}

#[test]
fn cell_clear_style_id() {
    let style_id = StyleId::new(50);
    let mut cell = Cell::with_style_id('B', style_id, CellFlags::empty());
    assert!(cell.uses_style_id());

    cell.clear_style_id();

    assert!(!cell.uses_style_id());
    assert!(cell.colors().is_default());
}

#[test]
fn cell_style_id_max_value() {
    // Test with maximum StyleId value
    let style_id = StyleId::new(u16::MAX);
    let cell = Cell::with_style_id('M', style_id, CellFlags::empty());
    assert!(cell.uses_style_id());
    assert_eq!(cell.style_id(), style_id);
}

#[test]
fn cell_with_style_id_wide_continuation() {
    let style_id = StyleId::new(10);
    let cell = Cell::with_style_id(' ', style_id, CellFlags::WIDE_CONTINUATION);
    assert!(cell.uses_style_id());
    assert!(cell.flags().contains(CellFlags::WIDE_CONTINUATION));
    assert_eq!(cell.style_id(), style_id);
}

#[test]
fn cell_flags_uses_style_id() {
    // Test the CellFlags::USES_STYLE_ID constant
    let flags = CellFlags::USES_STYLE_ID;
    assert!(flags.uses_style_id());
    assert!(!flags.is_complex());

    let combined = CellFlags::USES_STYLE_ID.union(CellFlags::WIDE);
    assert!(combined.uses_style_id());
    assert!(combined.contains(CellFlags::WIDE));
}

// =========================================================================
// HAS_EXTRAS flag tests (#5551)
// =========================================================================

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
fn cell_clear_resets_has_extras() {
    let mut cell = Cell::new('A');
    cell.set_has_extras(true);
    assert!(cell.has_extras());

    cell.clear();
    assert!(!cell.has_extras());
}

#[test]
fn cell_from_ascii_styled_with_extras_flag() {
    let colors = PackedColors::with_indexed(196, 21).with_extras_flag();
    let cell = Cell::from_ascii_styled(b'X', colors, CellFlags::empty());
    assert!(cell.has_extras());
    assert_eq!(cell.char(), 'X');
}

// =========================================================================
// Cell memory layout (#7649): the three fields sit at fixed bit offsets
// =========================================================================

/// Verify that the packed cell representation allows efficient single-load access
/// to char_data without any shift (it's at byte offset 0).
#[test]
fn cell_char_data_at_byte_offset_zero() {
    // Create a cell with a known char value and verify raw memory layout.
    let cell = Cell::from_ascii_fast(b'A');
    let raw: u64 = unsafe { std::mem::transmute(cell) };

    // char_data should be in the lowest 16 bits (byte offset 0).
    let extracted_char = (raw & 0xFFFF) as u16;
    assert_eq!(
        extracted_char, b'A' as u16,
        "char_data must be at bits [0..16)"
    );
}

/// Verify that colors field is at byte offset 2 (bit offset 16).
#[test]
fn cell_colors_at_byte_offset_two() {
    let colors = PackedColors::with_indexed(42, 99);
    let cell = Cell::from_ascii_styled(b'X', colors, CellFlags::empty());
    let raw: u64 = unsafe { std::mem::transmute(cell) };

    // Colors should be in bits [16..48).
    let extracted_colors = ((raw >> 16) & 0xFFFF_FFFF) as u32;
    assert_eq!(
        extracted_colors, colors.0,
        "colors must be at bits [16..48)"
    );
}

/// Verify that flags field is at byte offset 6 (bit offset 48).
#[test]
fn cell_flags_at_byte_offset_six() {
    let flags = CellFlags::BOLD.union(CellFlags::ITALIC);
    let cell = Cell::from_ascii_styled(b'Y', PackedColors::DEFAULT, flags);
    let raw: u64 = unsafe { std::mem::transmute(cell) };

    // Flags should be in bits [48..64).
    let extracted_flags = ((raw >> 48) & 0xFFFF) as u16;
    assert_eq!(
        extracted_flags,
        flags.bits(),
        "flags must be at bits [48..64)"
    );
}
