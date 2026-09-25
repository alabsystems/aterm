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
    // EMPTY / default / from_ascii_fast / from_ascii_styled / new / with_style,
    // and the from_raw_parts / from_checkpoint_raw round trips
    // =========================================================================

    /// Assert one side of `colors` reads back as the legacy `want` colour.
    fn assert_side_matches(
        label: &str,
        (default, indexed, rgb, index): (bool, bool, bool, u8),
        want: PackedColor,
    ) {
        if want.is_indexed() {
            assert!(!default && indexed && !rgb, "{label}: want indexed");
            assert_eq!(index, want.index(), "{label}: index");
        } else if want.is_rgb() {
            assert!(!default && !indexed && rgb, "{label}: want rgb");
        } else {
            assert!(default && !indexed && !rgb, "{label}: want default");
        }
    }

    #[test]
    fn value_constructors_store_exactly_what_they_are_given() {
        // One walk in place of 31 one-case tests.

        // The blank cell, three ways: a space, default colours, no flags.
        for blank in [Cell::EMPTY, Cell::default(), Cell::from_ascii_fast(b' ')] {
            assert_eq!(blank.char(), ' ');
            assert_eq!(blank.char_data(), u16::from(b' '));
            let colors = blank.colors();
            assert!(colors.is_default() && colors.fg_is_default() && colors.bg_is_default());
            assert!(blank.flags().is_empty());
            assert!(!blank.is_complex());
            assert!(!blank.is_wide() && !blank.is_wide_continuation());
            assert!(!blank.uses_style_id());
            assert!(blank.is_empty());
        }

        // Every ASCII byte (0x20 and 0x7E are the printable ends) through the
        // two byte constructors.
        let styles = [
            (PackedColors::DEFAULT, CellFlags::empty()),
            (
                PackedColors::with_indexed(196, 21),
                CellFlags::BOLD.union(CellFlags::ITALIC),
            ),
            (
                PackedColors::DEFAULT.with_rgb_fg().with_rgb_bg(),
                CellFlags::INVERSE,
            ),
        ];
        for byte in 0..=0x7Fu8 {
            let fast = Cell::from_ascii_fast(byte);
            assert_eq!(fast.char_data(), u16::from(byte));
            assert_eq!(fast.char(), char::from(byte));
            assert!(fast.colors().is_default());
            assert!(fast.flags().is_empty());
            for (colors, flags) in styles {
                let styled = Cell::from_ascii_styled(byte, colors, flags);
                assert_eq!(styled.char(), char::from(byte));
                assert_eq!(styled.colors(), colors);
                assert_eq!(styled.flags(), flags);
            }
        }

        // `new` and `with_style`: a BMP char (NUL, ASCII, CJK, hiragana, U+FFFF)
        // is stored as itself; one above the BMP (emoji, mathematical bold A) is
        // stored as U+FFFD. Neither sets COMPLEX.
        let chars = [
            ('\0', '\0'),
            ('A', 'A'),
            ('\u{4E16}', '\u{4E16}'),
            ('\u{3042}', '\u{3042}'),
            ('\u{FFFF}', '\u{FFFF}'),
            ('\u{1F600}', '\u{FFFD}'),
            ('\u{1D400}', '\u{FFFD}'),
        ];
        let fgs = [
            PackedColor::DEFAULT_FG,
            PackedColor::indexed(42),
            PackedColor::indexed(196),
            PackedColor::rgb(255, 0, 0),
        ];
        let bgs = [
            PackedColor::DEFAULT_BG,
            PackedColor::indexed(99),
            PackedColor::indexed(21),
            PackedColor::rgb(0, 0, 255),
        ];
        let flag_sets = [
            CellFlags::empty(),
            CellFlags::BOLD.union(CellFlags::ITALIC),
            CellFlags::BOLD.union(CellFlags::STRIKETHROUGH),
            CellFlags::WIDE,
            CellFlags::WIDE_CONTINUATION,
        ];
        for (input, stored) in chars {
            let plain = Cell::new(input);
            assert_eq!(plain.char(), stored, "new({input:?})");
            assert_eq!(u32::from(plain.char_data()), u32::from(stored));
            assert_eq!(plain.codepoint(), u32::from(stored));
            assert!(!plain.is_complex());
            assert!(plain.colors().is_default());
            assert!(plain.flags().is_empty());

            for fg in fgs {
                for bg in bgs {
                    for flags in flag_sets {
                        let cell = Cell::with_style(input, fg, bg, flags);
                        let label = format!("with_style({input:?}, {fg:?}, {bg:?}, {flags:?})");
                        assert_eq!(cell.char(), stored, "{label}");
                        assert_eq!(cell.flags(), flags, "{label}");
                        assert_eq!(cell.is_wide(), flags.contains(CellFlags::WIDE));
                        assert_eq!(
                            cell.is_wide_continuation(),
                            flags.contains(CellFlags::WIDE_CONTINUATION)
                        );
                        let colors = cell.colors();
                        let fg_read = (
                            colors.fg_is_default(),
                            colors.fg_is_indexed(),
                            colors.fg_is_rgb(),
                            colors.fg_index(),
                        );
                        let bg_read = (
                            colors.bg_is_default(),
                            colors.bg_is_indexed(),
                            colors.bg_is_rgb(),
                            colors.bg_index(),
                        );
                        assert_side_matches(&label, fg_read, fg);
                        assert_side_matches(&label, bg_read, bg);

                        // Both raw constructors rebuild the cell losslessly.
                        let raw = Cell::from_raw_parts(cell.char_data(), colors, cell.flags());
                        let checkpoint =
                            Cell::from_checkpoint_raw(cell.char_data(), cell.flags(), colors.0);
                        for back in [raw, checkpoint] {
                            assert_eq!(back.char_data(), cell.char_data(), "{label}");
                            assert_eq!(back.colors(), colors, "{label}");
                            assert_eq!(back.flags(), cell.flags(), "{label}");
                        }
                    }
                }
            }
        }
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
