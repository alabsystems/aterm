// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Cell constructor methods.

use super::{Cell, CellFlags, PackedColor, PackedColors};

impl Cell {
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

    /// Fast path: a styled cell from an ASCII byte, with no char translation or
    /// width check.
    ///
    /// # Preconditions (caller must verify)
    /// - `byte` is printable ASCII (0x20..=0x7E)
    /// - `colors` is already packed (no RGB overflow needed)
    #[must_use]
    #[inline]
    pub const fn from_ascii_styled(byte: u8, colors: PackedColors, flags: CellFlags) -> Self {
        Self {
            char_data: byte as u16,
            colors,
            flags,
        }
    }

    /// Create a cell from a character. A non-BMP character is stored as U+FFFD;
    /// its text needs the overflow mechanism.
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
            Self {
                char_data: '\u{FFFD}' as u16,
                colors: PackedColors::DEFAULT,
                flags: CellFlags::empty(),
            }
        }
    }

    /// Create a cell with colours and flags. For RGB, the colours mark RGB
    /// mode and the values live in the CellExtras overflow.
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

        let colors = Self::convert_legacy_colors(fg, bg);

        Self {
            char_data,
            colors,
            flags,
        }
    }

    /// Reconstruct a cell from its raw field values (checkpoint restore),
    /// bypassing validation so COMPLEX cells round-trip losslessly.
    #[must_use]
    #[inline]
    pub const fn from_raw_parts(char_data: u16, colors: PackedColors, flags: CellFlags) -> Self {
        Self {
            char_data,
            colors,
            flags,
        }
    }

    /// Convert a PackedColor pair to PackedColors (for bulk operations that
    /// pre-compute colours).
    #[must_use]
    #[inline]
    pub const fn convert_colors(fg: PackedColor, bg: PackedColor) -> PackedColors {
        Self::convert_legacy_colors(fg, bg)
    }

    /// Convert a PackedColor pair to PackedColors. RGB sets only the mode bits;
    /// the value lives in CellExtras.
    #[inline]
    const fn convert_legacy_colors(fg: PackedColor, bg: PackedColor) -> PackedColors {
        let mut colors = PackedColors::DEFAULT;

        if fg.is_indexed() {
            colors = colors.set_fg_indexed(fg.index());
        } else if fg.is_rgb() {
            colors = colors.with_rgb_fg();
        }

        if bg.is_indexed() {
            colors = colors.set_bg_indexed(bg.index());
        } else if bg.is_rgb() {
            colors = colors.with_rgb_bg();
        }

        colors
    }

    /// Create a cell with an overflow index for a complex character; the string
    /// lives in CellExtras.
    #[cfg(test)]
    #[must_use]
    #[inline]
    pub const fn with_overflow_index(index: u16) -> Self {
        Self {
            char_data: index,
            colors: PackedColors::DEFAULT,
            flags: CellFlags::COMPLEX,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cell, CellFlags, PackedColor, PackedColors};

    // EMPTY / default / from_ascii_fast / from_ascii_styled / new / with_style,
    // and the from_raw_parts round trip

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
        // The blank cell, three ways: a space, default colours, no flags.
        for blank in [Cell::EMPTY, Cell::default(), Cell::from_ascii_fast(b' ')] {
            assert_eq!(blank.char(), ' ');
            assert_eq!(blank.char_data(), u16::from(b' '));
            let colors = blank.colors();
            assert!(colors.is_default() && colors.fg_is_default() && colors.bg_is_default());
            assert!(blank.flags().is_empty());
            assert!(!blank.is_complex());
            assert!(!blank.is_wide() && !blank.is_wide_continuation());
            assert!(blank.is_empty());
        }

        // Every ASCII byte through the two byte constructors.
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

                        // The raw constructor rebuilds the cell losslessly.
                        let back = Cell::from_raw_parts(cell.char_data(), colors, cell.flags());
                        assert_eq!(back.char_data(), cell.char_data(), "{label}");
                        assert_eq!(back.colors(), colors, "{label}");
                        assert_eq!(back.flags(), cell.flags(), "{label}");
                    }
                }
            }
        }
    }
}
