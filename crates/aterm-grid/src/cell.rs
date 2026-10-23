// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Packed 8-byte terminal cell (`char_data`, `colors`, and `flags`).

pub use super::cell_colors::{PackedColor, PackedColors};
pub use super::cell_flags::CellFlags;

/// A single 8-byte terminal cell.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C, packed)]
pub struct Cell {
    /// A BMP code unit, or an overflow index when `COMPLEX` is set.
    char_data: u16,
    /// Packed foreground and background colors.
    colors: PackedColors,
    /// Cell flags including COMPLEX indicator.
    flags: CellFlags,
}

const _: () = assert!(std::mem::size_of::<Cell>() == 8);

impl Default for Cell {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl std::fmt::Debug for Cell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let char_data = self.char_data;
        let flags = self.flags;
        let colors = self.colors;
        if flags.is_complex() {
            f.debug_struct("Cell")
                .field("overflow_index", &char_data)
                .field("complex", &true)
                .field("flags", &flags)
                .field("colors", &colors)
                .finish()
        } else {
            let ch = char::from_u32(u32::from(char_data)).unwrap_or('\u{FFFD}');
            f.debug_struct("Cell")
                .field("char", &ch)
                .field("flags", &flags)
                .field("colors", &colors)
                .finish()
        }
    }
}

impl Cell {
    /// Maximum codepoint that fits directly in char_data (BMP).
    pub const MAX_DIRECT_CODEPOINT: u32 = 0xFFFF;

    /// Empty cell (space with default colors).
    pub const EMPTY: Self = Self {
        char_data: ' ' as u16,
        colors: PackedColors::DEFAULT,
        flags: CellFlags::empty(),
    };

    /// Creates a Background Color Erase (BCE) blank cell with the given background.
    #[must_use]
    #[inline]
    pub const fn bce_blank(bg_colors: PackedColors) -> Self {
        let bg_only = bg_colors.0 & 0xF000_FF00;
        if bg_only == 0 {
            Self::EMPTY
        } else {
            Self {
                char_data: ' ' as u16,
                colors: PackedColors(bg_only),
                flags: CellFlags::empty(),
            }
        }
    }

    /// Creates a BCE blank cell from a `PackedColor` background.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub const fn bce_blank_from_bg(bg: PackedColor) -> Self {
        if bg.is_default() {
            Self::EMPTY
        } else if bg.is_indexed() {
            Self::bce_blank(PackedColors::with_indexed_bg(bg.index()))
        } else {
            Self::bce_blank(PackedColors(0).with_rgb_bg())
        }
    }

    /// Returns the raw `char_data` value (BMP code unit or overflow table index).
    #[must_use]
    #[inline]
    pub const fn char_data(&self) -> u16 {
        self.char_data
    }

    /// Check if this cell uses overflow for its character.
    #[must_use]
    #[inline]
    pub const fn is_complex(&self) -> bool {
        let flags = self.flags;
        flags.is_complex()
    }

    /// The Unicode codepoint, or U+FFFD for a complex cell.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub const fn codepoint(&self) -> u32 {
        let flags = self.flags;
        if flags.is_complex() {
            0xFFFD
        } else {
            self.char_data as u32
        }
    }

    /// Returns the BMP character, or U+FFFD for a complex cell.
    #[must_use]
    #[inline]
    pub const fn char(&self) -> char {
        let flags = self.flags;
        let char_data = self.char_data;
        if flags.is_complex() {
            '\u{FFFD}' // Complex - use overflow table
        } else if let Some(ch) = char::from_u32(char_data as u32) {
            ch
        } else {
            '\u{FFFD}'
        }
    }

    /// Get the cell flags.
    #[must_use]
    #[inline]
    pub const fn flags(&self) -> CellFlags {
        self.flags
    }

    /// Get packed colors.
    #[must_use]
    #[inline]
    pub const fn colors(&self) -> PackedColors {
        self.colors
    }

    /// Sets a single-width BMP character (non-BMP characters use `set_overflow_index`).
    #[inline]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "cp verified <= 0xFFFF before cast; mask makes the cast provably lossless"
    )]
    pub(crate) const fn set_char(&mut self, c: char) {
        let cp = c as u32;
        debug_assert!(
            cp <= Self::MAX_DIRECT_CODEPOINT,
            "set_char() called with non-BMP character; use set_overflow_index() instead"
        );
        let char_data = if cp <= Self::MAX_DIRECT_CODEPOINT {
            (cp & 0xFFFF) as u16
        } else {
            0xFFFD // U+FFFD REPLACEMENT CHARACTER (non-BMP misuse fallback)
        };
        self.char_data = char_data;
        let mut flags = self.flags;
        flags.remove(CellFlags::COMPLEX);
        flags.remove(CellFlags::WIDE);
        flags.remove(CellFlags::WIDE_CONTINUATION);
        self.flags = flags;
    }

    /// Set overflow index for complex character.
    #[inline]
    pub(crate) const fn set_overflow_index(&mut self, index: u16) {
        self.char_data = index;
        let mut flags = self.flags;
        flags.insert(CellFlags::COMPLEX);
        self.flags = flags;
    }

    /// Set the flags.
    #[inline]
    pub const fn set_flags(&mut self, flags: CellFlags) {
        self.flags = flags;
    }

    /// Set the foreground color.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_fg(&mut self, fg: PackedColor) {
        let colors = self.colors;
        if fg.is_default() {
            self.colors = colors.set_fg_default();
        } else if fg.is_indexed() {
            self.colors = colors.set_fg_indexed(fg.index());
        } else {
            // RGB: the value itself lives in `CellExtras`.
            self.colors = colors.with_rgb_fg();
        }
    }

    /// Check if this is a wide character.
    #[must_use]
    #[inline]
    pub const fn is_wide(&self) -> bool {
        let flags = self.flags;
        flags.contains(CellFlags::WIDE)
    }

    /// Check if this is a wide character continuation.
    #[must_use]
    #[inline]
    pub const fn is_wide_continuation(&self) -> bool {
        let flags = self.flags;
        flags.contains(CellFlags::WIDE_CONTINUATION)
    }

    /// Checks if this cell has the `PROTECTED` / `WIDE_CONTINUATION` bit set.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub const fn is_protected(&self) -> bool {
        let flags = self.flags;
        flags.contains(CellFlags::PROTECTED)
    }

    /// Check if this cell has a CellExtras entry; cells without one skip the probe.
    #[must_use]
    #[inline]
    pub const fn has_extras(&self) -> bool {
        let colors = self.colors;
        colors.has_extras()
    }

    /// Set or clear the HAS_EXTRAS flag on this cell.
    #[inline]
    pub(crate) const fn set_has_extras(&mut self, has: bool) {
        let colors = self.colors;
        self.colors = if has {
            colors.with_extras_flag()
        } else {
            colors.without_extras_flag()
        };
    }

    /// Check if this cell is empty (space with default colors and no special flags).
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        let colors = self.colors;
        let flags = self.flags;
        self.char_data == ' ' as u16 && colors.is_default() && flags.0 == 0
    }
}

#[path = "cell_constructors.rs"]
mod constructors;

#[path = "cell_color_accessors.rs"]
mod color_accessors;

#[cfg(test)]
#[path = "cell_tests.rs"]
mod tests;
