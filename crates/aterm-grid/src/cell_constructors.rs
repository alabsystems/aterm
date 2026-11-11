// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Cell constructor methods.

use super::{Cell, CellFlags, PackedColor, PackedColors};

impl Cell {
    /// Create a cell from an ASCII byte (hot path, no checks).
    #[must_use]
    #[inline]
    pub const fn from_ascii_fast(byte: u8) -> Self {
        Self {
            char_data: byte as u16,
            colors: PackedColors::DEFAULT,
            flags: CellFlags::empty(),
        }
    }

    /// A styled cell from an ASCII byte, with no char translation or width check.
    #[must_use]
    #[inline]
    pub const fn from_ascii_styled(byte: u8, colors: PackedColors, flags: CellFlags) -> Self {
        Self {
            char_data: byte as u16,
            colors,
            flags,
        }
    }

    /// Create a cell from a character.
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

    /// Create a cell with colours and flags.
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

    /// Reconstruct a cell from its raw field values.
    #[must_use]
    #[inline]
    pub const fn from_raw_parts(char_data: u16, colors: PackedColors, flags: CellFlags) -> Self {
        Self {
            char_data,
            colors,
            flags,
        }
    }

    /// Convert a PackedColor pair to PackedColors (for bulk operations that pre-compute colours).
    #[must_use]
    #[inline]
    pub const fn convert_colors(fg: PackedColor, bg: PackedColor) -> PackedColors {
        Self::convert_legacy_colors(fg, bg)
    }

    /// Convert a PackedColor pair to PackedColors.
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

    /// Create a cell with an overflow index for a complex character.
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
#[path = "cell_constructors_tests.rs"]
mod tests;
