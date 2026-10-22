// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Packed 8-byte cell (16 MB for 10,000 lines x 200 cols).
//!
//! - `char_data` (2 bytes): the UTF-16 code unit of a BMP character, or an
//!   overflow table index when `COMPLEX` is set.
//! - `colors` (4 bytes, [`PackedColors`]): FG index in bits 0-7 and BG index in
//!   bits 8-15 (a StyleId in bits 0-15 when `USES_STYLE_ID` is set), the
//!   has-extras bit 16, FG mode in bits 24-27 and BG mode in bits 28-31.
//! - `flags` (2 bytes, [`CellFlags`]): attributes in bits 0-13, `USES_STYLE_ID`
//!   in bit 14, `COMPLEX` in bit 15.
//!
//! Every cell is 8 bytes (16 MB for 10,000 lines x 200 cols).
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────────┐
//! │ char_data (2 bytes)                                            │
//! │   - UTF-16 code unit for BMP (U+0000-U+FFFF)                   │
//! │   - Overflow table index when flags.COMPLEX is set             │
//! ├────────────────────────────────────────────────────────────────┤
//! │ colors (4 bytes) - Packed foreground and background            │
//! │   - Bits 0-7:   FG color index (0-255) or FG mode indicator    │
//! │   - Bits 8-15:  BG color index (0-255) or BG mode indicator    │
//! │   - Bits 16-23: Extra color data / overflow indicator          │
//! │   - Bits 24-31: Color mode flags                               │
//! ├────────────────────────────────────────────────────────────────┤
//! │ flags (2 bytes) - Cell attributes                              │
//! │   - Bits 0-14: Standard attributes                             │
//! │   - Bit 15: COMPLEX flag (char_data is overflow index)         │
//! └────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Overflow Tables (in CellExtra)
//!
//! When a cell needs more than 8 bytes can express:
//! - Complex characters (emoji, combining marks, non-BMP): string table
//! - True color RGB: separate fg/bg overflow tables
//! - Hyperlinks: URL storage
//! - Underline color: separate color storage
//!
//! Few cells need overflow.
//!
//! ## Verification
//!
//! - Exhaustive tests: `cell_tests.rs`
//!   (`cell_packing_is_lossless_over_every_input`: every char, every visual
//!   flag set, every indexed colour pair and every overflow index).
//! - Compile-time assert: `size_of::<Cell>() == 8`

pub use super::cell_colors::{PackedColor, PackedColors};
pub use super::cell_flags::CellFlags;

// Constructors (from_ascii_fast, from_ascii_styled, new, with_style,
// with_overflow_index) and color
// conversion helpers are in cell_constructors.rs.

/// A single terminal cell (8 bytes). See module docs for layout details.
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

    /// A BCE (Background Color Erase) blank, as VT420/xterm erases fill: a space
    /// with default foreground, the given background and no flags. A default
    /// background gives `Cell::EMPTY`.
    #[must_use]
    #[inline]
    pub const fn bce_blank(bg_colors: PackedColors) -> Self {
        // Keep only BG mode (bits 28-31) and BG index (bits 8-15).
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

    /// Create a BCE blank cell from a `PackedColor` background. For RGB only the
    /// mode bit is set; the RGB value is written to `CellExtras` per position.
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

    // Constructors in cell_constructors.rs: from_ascii_fast, from_ascii_styled,
    // new, with_style, convert_colors, convert_legacy_colors, with_overflow_index.

    /// Get the raw char_data value.
    ///
    /// If `is_complex()`, this is an overflow table index.
    /// Otherwise, it's a UTF-16 code unit (BMP codepoint).
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

    /// Get the character, or U+FFFD for a complex cell (non-BMP character or
    /// grapheme cluster), whose text lives in `CellExtras`.
    ///
    /// [`Grid::resolved_char(row, col)`](crate::Grid::resolved_char) resolves
    /// complex cells.
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

    /// Set the character (BMP only). A non-BMP character stores U+FFFD and trips a
    /// deliberate misuse `debug_assert!` (Trust's L0 gate reports it as an accepted
    /// refutation at `cp = 0x10000`); those go through `set_overflow_index`.
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
        // set_char writes a single-width BMP character, so clear COMPLEX, WIDE and
        // WIDE_CONTINUATION; otherwise DECFRA over wide characters leaves stale WIDE
        // flags, and over continuations a stale bit 10 that reads as PROTECTED.
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

    /// Check if this cell is protected from selective erase.
    ///
    /// `PROTECTED` shares bit 10 with `WIDE_CONTINUATION`, so this is also true for
    /// every wide-continuation spacer; `Row::is_cell_protected()` disambiguates.
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
