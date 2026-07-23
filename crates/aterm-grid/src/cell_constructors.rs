// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Cell constructor methods.
//!
//! Extracted from `cell.rs` to keep the main file focused on accessors
//! and predicates.

#[cfg(any(test, kani, feature = "testing"))]
use super::super::style::StyleId;
use super::{Cell, CellFlags, PackedColor, PackedColors};

impl Cell {
    /// Rebuild a cell from raw checkpoint storage.
    ///
    /// This preserves the in-memory cell layout exactly, including
    /// `USES_STYLE_ID` and `COMPLEX` payloads.
    #[must_use]
    #[inline]
    pub const fn from_checkpoint_raw(char_data: u16, flags: CellFlags, colors_raw: u32) -> Self {
        Self {
            char_data,
            colors: PackedColors(colors_raw),
            flags,
        }
    }

    /// Create a cell from an ASCII byte (hot path, no checks).
    /// Precondition: `byte` is printable ASCII (0x20..=0x7E).
    #[must_use]
    #[inline]
    pub const fn from_ascii_fast(byte: u8) -> Self {
        Self {
            char_data: byte as u16,
            colors: PackedColors::DEFAULT,
            flags: CellFlags::empty(),
        }
    }

    /// FAST PATH: Create a styled cell from an ASCII byte.
    ///
    /// # Preconditions (caller must verify)
    /// - `byte` is printable ASCII (0x20..=0x7E)
    /// - `colors` is already packed (no RGB overflow needed)
    ///
    /// This creates a Cell directly without char translation or width checks,
    /// ideal for bulk ASCII writes with a known style.
    #[must_use]
    #[inline]
    pub const fn from_ascii_styled(byte: u8, colors: PackedColors, flags: CellFlags) -> Self {
        Self {
            char_data: byte as u16,
            colors,
            flags,
        }
    }

    /// Create a new cell from a character.
    ///
    /// For BMP characters (U+0000-U+FFFF), stores directly.
    /// For non-BMP characters, caller should use overflow mechanism.
    #[must_use]
    #[inline]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "cp verified <= 0xFFFF before cast"
    )]
    pub const fn new(c: char) -> Self {
        let cp = c as u32;
        if cp <= Self::MAX_DIRECT_CODEPOINT {
            Self {
                char_data: cp as u16,
                colors: PackedColors::DEFAULT,
                flags: CellFlags::empty(),
            }
        } else {
            // Non-BMP character - store replacement char, caller should use overflow
            Self {
                char_data: '\u{FFFD}' as u16,
                colors: PackedColors::DEFAULT,
                flags: CellFlags::empty(),
            }
        }
    }

    /// Create a new cell with colors and flags.
    ///
    /// Note: For RGB colors, the colors should be set up to indicate RGB mode,
    /// and actual RGB values stored in CellExtras overflow.
    #[must_use]
    #[inline]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "cp verified <= 0xFFFF before cast"
    )]
    pub const fn with_style(c: char, fg: PackedColor, bg: PackedColor, flags: CellFlags) -> Self {
        let cp = c as u32;
        let char_data = if cp <= Self::MAX_DIRECT_CODEPOINT {
            cp as u16
        } else {
            '\u{FFFD}' as u16
        };

        // Convert legacy PackedColor to PackedColors
        let colors = Self::convert_legacy_colors(fg, bg);

        Self {
            char_data,
            colors,
            flags,
        }
    }

    /// Reconstruct a cell from its raw field values (checkpoint restore, #6030).
    ///
    /// This bypasses codepoint validation and color conversion, restoring the
    /// exact Cell representation that was serialized. Required for lossless
    /// round-trip of COMPLEX cells (overflow indices) and StyleId cells.
    #[must_use]
    #[inline]
    pub const fn from_raw_parts(char_data: u16, colors: PackedColors, flags: CellFlags) -> Self {
        Self {
            char_data,
            colors,
            flags,
        }
    }

    /// Convert PackedColor pair to PackedColors format.
    ///
    /// Public helper for bulk operations that need to pre-compute colors.
    #[must_use]
    #[inline]
    pub const fn convert_colors(fg: PackedColor, bg: PackedColor) -> PackedColors {
        Self::convert_legacy_colors(fg, bg)
    }

    /// Convert legacy PackedColor pair to new PackedColors format.
    #[inline]
    const fn convert_legacy_colors(fg: PackedColor, bg: PackedColor) -> PackedColors {
        let mut colors = PackedColors::DEFAULT;

        // Handle foreground
        if fg.is_indexed() {
            colors = colors.set_fg_indexed(fg.index());
        } else if fg.is_rgb() {
            // RGB needs overflow - mark as RGB mode
            colors = colors.with_rgb_fg();
        }
        // else: default

        // Handle background
        if bg.is_indexed() {
            colors = colors.set_bg_indexed(bg.index());
        } else if bg.is_rgb() {
            // RGB needs overflow - mark as RGB mode
            colors = colors.with_rgb_bg();
        }
        // else: default

        colors
    }

    /// Create a cell with overflow index for complex character (test/kani-only).
    ///
    /// The actual character string is stored in CellExtras.
    #[cfg(any(test, kani, feature = "testing"))]
    #[must_use]
    #[inline]
    pub const fn with_overflow_index(index: u16) -> Self {
        Self {
            char_data: index,
            colors: PackedColors::DEFAULT,
            flags: CellFlags::COMPLEX,
        }
    }

    /// Create a cell with a StyleId reference instead of inline colors.
    ///
    /// This is the Ghostty-style approach for memory-efficient style storage.
    /// The StyleId references a style in the StyleTable, which stores the
    /// actual colors and attributes.
    ///
    /// The `cell_flags` parameter should contain cell-specific flags only
    /// (WIDE, WIDE_CONTINUATION, PROTECTED). Style attributes (BOLD, ITALIC,
    /// etc.) are stored in the StyleTable and will be retrieved at render time.
    ///
    /// # Memory Layout
    ///
    /// When using StyleId:
    /// - `colors.0` low 16 bits: StyleId value
    /// - `colors.0` high 16 bits: reserved (for RGB overflow index)
    /// - `flags`: has USES_STYLE_ID set, plus cell-specific flags
    #[cfg(any(test, kani, feature = "testing"))]
    #[must_use]
    #[inline]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "cp verified <= 0xFFFF before cast"
    )]
    pub const fn with_style_id(c: char, style_id: StyleId, cell_flags: CellFlags) -> Self {
        let cp = c as u32;
        let char_data = if cp <= Self::MAX_DIRECT_CODEPOINT {
            cp as u16
        } else {
            '\u{FFFD}' as u16
        };

        // Store StyleId in the colors field's low 16 bits
        // Set USES_STYLE_ID flag to indicate this cell uses style interning
        let colors = PackedColors(style_id.raw() as u32);
        let flags = CellFlags(cell_flags.0 | CellFlags::USES_STYLE_ID.0);

        Self {
            char_data,
            colors,
            flags,
        }
    }

    /// Create a styled cell from an ASCII byte with StyleId.
    ///
    /// # Preconditions (caller must verify)
    /// - `byte` is printable ASCII (0x20..=0x7E)
    ///
    /// This is the hot path for ASCII output with style interning.
    #[cfg(any(test, kani, feature = "testing"))]
    #[must_use]
    #[inline]
    pub const fn from_ascii_with_style_id(
        byte: u8,
        style_id: StyleId,
        cell_flags: CellFlags,
    ) -> Self {
        let colors = PackedColors(style_id.raw() as u32);
        let flags = CellFlags(cell_flags.0 | CellFlags::USES_STYLE_ID.0);

        Self {
            char_data: byte as u16,
            colors,
            flags,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::style::StyleId;
    use super::{Cell, CellFlags, PackedColor, PackedColors};

    // =========================================================================
    // Cell::EMPTY / Cell::default() — blank cell invariants
    // =========================================================================

    #[test]
    fn test_empty_is_space_char() {
        assert_eq!(Cell::EMPTY.char(), ' ');
        assert_eq!(Cell::EMPTY.char_data(), ' ' as u16);
    }

    #[test]
    fn test_empty_has_default_colors() {
        let colors = Cell::EMPTY.colors();
        assert!(colors.is_default());
        assert!(colors.fg_is_default());
        assert!(colors.bg_is_default());
    }

    #[test]
    fn test_empty_has_no_flags() {
        assert!(Cell::EMPTY.flags().is_empty());
        assert!(!Cell::EMPTY.is_complex());
        assert!(!Cell::EMPTY.is_wide());
        assert!(!Cell::EMPTY.is_wide_continuation());
        assert!(!Cell::EMPTY.uses_style_id());
    }

    #[test]
    fn test_empty_is_empty() {
        assert!(Cell::EMPTY.is_empty());
    }

    #[test]
    fn test_default_equals_empty() {
        let def = Cell::default();
        assert_eq!(def.char_data(), Cell::EMPTY.char_data());
        assert_eq!(def.colors(), Cell::EMPTY.colors());
        assert_eq!(def.flags(), Cell::EMPTY.flags());
        assert!(def.is_empty());
    }

    // =========================================================================
    // from_ascii_fast — ASCII hot path
    // =========================================================================

    #[test]
    fn test_from_ascii_fast_stores_byte_as_char_data() {
        let cell = Cell::from_ascii_fast(b'A');
        assert_eq!(cell.char_data(), b'A' as u16);
        assert_eq!(cell.char(), 'A');
    }

    #[test]
    fn test_from_ascii_fast_default_colors() {
        let cell = Cell::from_ascii_fast(b'z');
        assert!(cell.colors().is_default());
    }

    #[test]
    fn test_from_ascii_fast_empty_flags() {
        let cell = Cell::from_ascii_fast(b'~');
        assert!(cell.flags().is_empty());
    }

    #[test]
    fn test_from_ascii_fast_space_matches_empty() {
        let cell = Cell::from_ascii_fast(b' ');
        assert_eq!(cell.char_data(), Cell::EMPTY.char_data());
        assert_eq!(cell.colors(), Cell::EMPTY.colors());
        assert_eq!(cell.flags(), Cell::EMPTY.flags());
    }

    #[test]
    fn test_from_ascii_fast_printable_range_boundaries() {
        // Lowest printable: space (0x20)
        let lo = Cell::from_ascii_fast(0x20);
        assert_eq!(lo.char(), ' ');

        // Highest printable: tilde (0x7E)
        let hi = Cell::from_ascii_fast(0x7E);
        assert_eq!(hi.char(), '~');
    }

    // =========================================================================
    // from_ascii_styled — ASCII with style
    // =========================================================================

    #[test]
    fn test_from_ascii_styled_preserves_byte() {
        let cell = Cell::from_ascii_styled(b'X', PackedColors::DEFAULT, CellFlags::empty());
        assert_eq!(cell.char(), 'X');
    }

    #[test]
    fn test_from_ascii_styled_preserves_colors() {
        let colors = PackedColors::with_indexed(196, 21);
        let cell = Cell::from_ascii_styled(b'A', colors, CellFlags::empty());
        assert!(cell.colors().fg_is_indexed());
        assert_eq!(cell.colors().fg_index(), 196);
        assert!(cell.colors().bg_is_indexed());
        assert_eq!(cell.colors().bg_index(), 21);
    }

    #[test]
    fn test_from_ascii_styled_preserves_flags() {
        let flags = CellFlags::BOLD.union(CellFlags::ITALIC);
        let cell = Cell::from_ascii_styled(b'B', PackedColors::DEFAULT, flags);
        assert!(cell.flags().contains(CellFlags::BOLD));
        assert!(cell.flags().contains(CellFlags::ITALIC));
    }

    // =========================================================================
    // Cell::new() — BMP and non-BMP character handling
    // =========================================================================

    #[test]
    fn test_new_ascii_char() {
        let cell = Cell::new('A');
        assert_eq!(cell.char(), 'A');
        assert_eq!(cell.char_data(), 0x0041);
        assert!(!cell.is_complex());
    }

    #[test]
    fn test_new_cjk_bmp_char() {
        // U+4E16 = 世 (CJK, within BMP)
        let cell = Cell::new('\u{4E16}');
        assert_eq!(cell.char(), '\u{4E16}');
        assert_eq!(cell.char_data(), 0x4E16);
    }

    #[test]
    fn test_new_max_bmp_codepoint() {
        let cell = Cell::new('\u{FFFF}');
        assert_eq!(cell.char_data(), 0xFFFF);
        assert!(!cell.is_complex());
    }

    #[test]
    fn test_new_non_bmp_stores_replacement() {
        // Emoji U+1F600 is above BMP
        let cell = Cell::new('\u{1F600}');
        assert_eq!(cell.char(), '\u{FFFD}');
        assert_eq!(cell.char_data(), '\u{FFFD}' as u16);
    }

    #[test]
    fn test_new_default_colors_and_no_flags() {
        let cell = Cell::new('Q');
        assert!(cell.colors().is_default());
        assert!(cell.flags().is_empty());
    }

    #[test]
    fn test_new_null_char() {
        let cell = Cell::new('\0');
        assert_eq!(cell.char_data(), 0);
        assert_eq!(cell.codepoint(), 0);
    }

    // =========================================================================
    // Cell::with_style() — character + colors + flags
    // =========================================================================

    #[test]
    fn test_with_style_bmp_char_preserved() {
        let cell = Cell::with_style(
            'Z',
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        assert_eq!(cell.char(), 'Z');
    }

    #[test]
    fn test_with_style_non_bmp_stores_replacement() {
        let cell = Cell::with_style(
            '\u{1D400}', // 𝐀 (mathematical bold A, non-BMP)
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        assert_eq!(cell.char(), '\u{FFFD}');
    }

    #[test]
    fn test_with_style_indexed_fg_converted() {
        let cell = Cell::with_style(
            'X',
            PackedColor::indexed(42),
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        assert!(cell.colors().fg_is_indexed());
        assert_eq!(cell.colors().fg_index(), 42);
        assert!(cell.colors().bg_is_default());
    }

    #[test]
    fn test_with_style_indexed_bg_converted() {
        let cell = Cell::with_style(
            'X',
            PackedColor::DEFAULT_FG,
            PackedColor::indexed(99),
            CellFlags::empty(),
        );
        assert!(cell.colors().fg_is_default());
        assert!(cell.colors().bg_is_indexed());
        assert_eq!(cell.colors().bg_index(), 99);
    }

    #[test]
    fn test_with_style_both_indexed_colors() {
        let cell = Cell::with_style(
            'X',
            PackedColor::indexed(196),
            PackedColor::indexed(21),
            CellFlags::empty(),
        );
        assert!(cell.colors().fg_is_indexed());
        assert_eq!(cell.colors().fg_index(), 196);
        assert!(cell.colors().bg_is_indexed());
        assert_eq!(cell.colors().bg_index(), 21);
    }

    #[test]
    fn test_with_style_rgb_fg_marks_overflow() {
        let cell = Cell::with_style(
            'X',
            PackedColor::rgb(255, 0, 0),
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        assert!(cell.colors().fg_is_rgb());
        assert!(cell.colors().bg_is_default());
    }

    #[test]
    fn test_with_style_rgb_bg_marks_overflow() {
        let cell = Cell::with_style(
            'X',
            PackedColor::DEFAULT_FG,
            PackedColor::rgb(0, 0, 255),
            CellFlags::empty(),
        );
        assert!(cell.colors().fg_is_default());
        assert!(cell.colors().bg_is_rgb());
    }

    #[test]
    fn test_with_style_flags_bold_italic() {
        let flags = CellFlags::BOLD.union(CellFlags::ITALIC);
        let cell = Cell::with_style('X', PackedColor::DEFAULT_FG, PackedColor::DEFAULT_BG, flags);
        assert!(cell.flags().contains(CellFlags::BOLD));
        assert!(cell.flags().contains(CellFlags::ITALIC));
        assert!(!cell.flags().contains(CellFlags::UNDERLINE));
    }

    #[test]
    fn test_with_style_wide_flag() {
        let cell = Cell::with_style(
            '\u{3042}', // あ (hiragana a, CJK)
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::WIDE,
        );
        assert!(cell.is_wide());
        assert_eq!(cell.char(), '\u{3042}');
    }

    #[test]
    fn test_with_style_continuation_flag() {
        let cell = Cell::with_style(
            ' ',
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::WIDE_CONTINUATION,
        );
        assert!(cell.is_wide_continuation());
    }

    // =========================================================================
    // from_raw_parts / from_checkpoint_raw — lossless round-trip
    // =========================================================================

    #[test]
    fn test_from_raw_parts_roundtrip() {
        let original = Cell::with_style(
            'Q',
            PackedColor::indexed(100),
            PackedColor::indexed(200),
            CellFlags::BOLD.union(CellFlags::STRIKETHROUGH),
        );
        let restored =
            Cell::from_raw_parts(original.char_data(), original.colors(), original.flags());
        assert_eq!(restored.char_data(), original.char_data());
        assert_eq!(restored.colors(), original.colors());
        assert_eq!(restored.flags(), original.flags());
    }

    #[test]
    fn test_from_checkpoint_raw_roundtrip() {
        let original =
            Cell::from_ascii_styled(b'H', PackedColors::with_indexed(7, 0), CellFlags::INVERSE);
        let restored =
            Cell::from_checkpoint_raw(original.char_data(), original.flags(), original.colors().0);
        assert_eq!(restored.char_data(), original.char_data());
        assert_eq!(restored.colors(), original.colors());
        assert_eq!(restored.flags(), original.flags());
    }

    // =========================================================================
    // convert_colors / convert_legacy_colors
    // =========================================================================

    #[test]
    fn test_convert_colors_default_default() {
        let packed = Cell::convert_colors(PackedColor::DEFAULT_FG, PackedColor::DEFAULT_BG);
        assert!(packed.is_default());
    }

    #[test]
    fn test_convert_colors_indexed_fg() {
        let packed = Cell::convert_colors(PackedColor::indexed(42), PackedColor::DEFAULT_BG);
        assert!(packed.fg_is_indexed());
        assert_eq!(packed.fg_index(), 42);
        assert!(packed.bg_is_default());
    }

    #[test]
    fn test_convert_colors_indexed_bg() {
        let packed = Cell::convert_colors(PackedColor::DEFAULT_FG, PackedColor::indexed(99));
        assert!(packed.fg_is_default());
        assert!(packed.bg_is_indexed());
        assert_eq!(packed.bg_index(), 99);
    }

    #[test]
    fn test_convert_colors_rgb_fg_marks_rgb_mode() {
        let packed = Cell::convert_colors(PackedColor::rgb(10, 20, 30), PackedColor::DEFAULT_BG);
        assert!(packed.fg_is_rgb());
        assert!(packed.bg_is_default());
    }

    #[test]
    fn test_convert_colors_rgb_bg_marks_rgb_mode() {
        let packed = Cell::convert_colors(PackedColor::DEFAULT_FG, PackedColor::rgb(10, 20, 30));
        assert!(packed.fg_is_default());
        assert!(packed.bg_is_rgb());
    }

    // =========================================================================
    // with_overflow_index — complex character overflow
    // =========================================================================

    #[test]
    fn test_with_overflow_index_sets_complex_flag() {
        let cell = Cell::with_overflow_index(0);
        assert!(cell.is_complex());
        assert!(cell.flags().contains(CellFlags::COMPLEX));
    }

    #[test]
    fn test_with_overflow_index_stores_index() {
        let cell = Cell::with_overflow_index(42);
        assert_eq!(cell.char_data(), 42);
    }

    #[test]
    fn test_with_overflow_index_max_value() {
        let cell = Cell::with_overflow_index(u16::MAX);
        assert!(cell.is_complex());
        assert_eq!(cell.char_data(), u16::MAX);
    }

    #[test]
    fn test_with_overflow_index_default_colors() {
        let cell = Cell::with_overflow_index(7);
        assert!(cell.colors().is_default());
    }

    #[test]
    fn test_with_overflow_index_returns_replacement_char() {
        let cell = Cell::with_overflow_index(100);
        assert_eq!(cell.char(), '\u{FFFD}');
        assert_eq!(cell.codepoint(), 0xFFFD);
    }

    // =========================================================================
    // with_style_id — StyleId interning
    // =========================================================================

    #[test]
    fn test_with_style_id_sets_uses_style_id_flag() {
        let cell = Cell::with_style_id('A', StyleId::DEFAULT, CellFlags::empty());
        assert!(cell.uses_style_id());
        assert!(cell.flags().contains(CellFlags::USES_STYLE_ID));
    }

    #[test]
    fn test_with_style_id_stores_style_id_in_colors() {
        let sid = StyleId::new(42);
        let cell = Cell::with_style_id('A', sid, CellFlags::empty());
        assert_eq!(cell.style_id(), sid);
        assert_eq!(cell.style_id().raw(), 42);
    }

    #[test]
    fn test_with_style_id_preserves_character() {
        let cell = Cell::with_style_id('W', StyleId::new(5), CellFlags::empty());
        assert_eq!(cell.char(), 'W');
    }

    #[test]
    fn test_with_style_id_non_bmp_stores_replacement() {
        let cell = Cell::with_style_id('\u{1F600}', StyleId::new(1), CellFlags::empty());
        assert_eq!(cell.char(), '\u{FFFD}');
        assert!(cell.uses_style_id());
    }

    #[test]
    fn test_with_style_id_merges_cell_flags() {
        let cell = Cell::with_style_id('A', StyleId::new(1), CellFlags::WIDE);
        assert!(cell.flags().contains(CellFlags::WIDE));
        assert!(cell.flags().contains(CellFlags::USES_STYLE_ID));
    }

    #[test]
    fn test_with_style_id_max_style_id() {
        let sid = StyleId::new(u16::MAX);
        let cell = Cell::with_style_id('M', sid, CellFlags::empty());
        assert_eq!(cell.style_id(), sid);
        assert_eq!(cell.style_id().raw(), u16::MAX);
    }

    // =========================================================================
    // from_ascii_with_style_id — ASCII + StyleId hot path
    // =========================================================================

    #[test]
    fn test_from_ascii_with_style_id_stores_byte() {
        let cell = Cell::from_ascii_with_style_id(b'H', StyleId::new(5), CellFlags::empty());
        assert_eq!(cell.char(), 'H');
        assert_eq!(cell.char_data(), b'H' as u16);
    }

    #[test]
    fn test_from_ascii_with_style_id_sets_style() {
        let sid = StyleId::new(99);
        let cell = Cell::from_ascii_with_style_id(b'X', sid, CellFlags::empty());
        assert!(cell.uses_style_id());
        assert_eq!(cell.style_id(), sid);
    }

    #[test]
    fn test_from_ascii_with_style_id_merges_flags() {
        let cell =
            Cell::from_ascii_with_style_id(b'Z', StyleId::new(1), CellFlags::WIDE_CONTINUATION);
        assert!(cell.flags().contains(CellFlags::WIDE_CONTINUATION));
        assert!(cell.flags().contains(CellFlags::USES_STYLE_ID));
    }

    // =========================================================================
    // Packed representation correctness — 8-byte invariant
    // =========================================================================

    #[test]
    fn test_cell_is_8_bytes() {
        assert_eq!(std::mem::size_of::<Cell>(), 8);
    }

    #[test]
    fn test_all_constructors_produce_8_byte_cells() {
        let cells = [
            Cell::EMPTY,
            Cell::from_ascii_fast(b'A'),
            Cell::from_ascii_styled(b'B', PackedColors::DEFAULT, CellFlags::BOLD),
            Cell::new('C'),
            Cell::with_style(
                'D',
                PackedColor::indexed(1),
                PackedColor::indexed(2),
                CellFlags::DIM,
            ),
            Cell::from_raw_parts(0x41, PackedColors::DEFAULT, CellFlags::empty()),
            Cell::from_checkpoint_raw(0x42, CellFlags::empty(), 0),
            Cell::with_overflow_index(0),
            Cell::with_style_id('E', StyleId::new(1), CellFlags::empty()),
            Cell::from_ascii_with_style_id(b'F', StyleId::new(2), CellFlags::empty()),
        ];
        for (i, cell) in cells.iter().enumerate() {
            assert_eq!(
                std::mem::size_of_val(cell),
                8,
                "constructor at index {i} produced a cell != 8 bytes"
            );
        }
    }

    // =========================================================================
    // Cross-constructor consistency
    // =========================================================================

    #[test]
    fn test_from_ascii_fast_matches_new_for_ascii() {
        let via_fast = Cell::from_ascii_fast(b'Z');
        let via_new = Cell::new('Z');
        assert_eq!(via_fast.char_data(), via_new.char_data());
        assert_eq!(via_fast.colors(), via_new.colors());
        assert_eq!(via_fast.flags(), via_new.flags());
    }

    #[test]
    fn test_from_raw_parts_preserves_complex_cell() {
        let complex = Cell::with_overflow_index(999);
        let restored = Cell::from_raw_parts(complex.char_data(), complex.colors(), complex.flags());
        assert_eq!(restored.char_data(), 999);
        assert!(restored.is_complex());
    }
}
