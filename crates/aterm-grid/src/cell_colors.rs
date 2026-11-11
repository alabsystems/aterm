// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Packed color types for 8-byte terminal cells.

/// Foreground and background colours packed into 4 bytes of an 8-byte cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct PackedColors(pub u32);

impl PackedColors {
    const FG_MODE_SHIFT: u32 = 24;
    const BG_MODE_SHIFT: u32 = 28;
    const MODE_MASK: u32 = 0x0F;

    const MODE_DEFAULT: u32 = 0;
    const MODE_INDEXED: u32 = 1;
    const MODE_RGB: u32 = 2;

    /// Both colors are default.
    pub const DEFAULT: Self = Self(0);

    /// Create with default foreground and background.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub const fn new() -> Self {
        Self::DEFAULT
    }

    /// Create with indexed foreground and default background.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub const fn with_indexed_fg(fg_index: u8) -> Self {
        Self((Self::MODE_INDEXED << Self::FG_MODE_SHIFT) | (fg_index as u32))
    }

    /// Create with indexed background and default foreground.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub const fn with_indexed_bg(bg_index: u8) -> Self {
        Self((Self::MODE_INDEXED << Self::BG_MODE_SHIFT) | ((bg_index as u32) << 8))
    }

    /// Create with both indexed colors.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub const fn with_indexed(fg_index: u8, bg_index: u8) -> Self {
        Self(
            (Self::MODE_INDEXED << Self::FG_MODE_SHIFT)
                | (Self::MODE_INDEXED << Self::BG_MODE_SHIFT)
                | (fg_index as u32)
                | ((bg_index as u32) << 8),
        )
    }

    /// Mark foreground as RGB (actual color in overflow table).
    #[must_use]
    #[inline]
    pub const fn with_rgb_fg(self) -> Self {
        Self(
            (self.0 & !(Self::MODE_MASK << Self::FG_MODE_SHIFT))
                | (Self::MODE_RGB << Self::FG_MODE_SHIFT),
        )
    }

    /// Mark background as RGB (actual color in overflow table).
    #[must_use]
    #[inline]
    pub const fn with_rgb_bg(self) -> Self {
        Self(
            (self.0 & !(Self::MODE_MASK << Self::BG_MODE_SHIFT))
                | (Self::MODE_RGB << Self::BG_MODE_SHIFT),
        )
    }

    /// Get foreground color mode.
    #[must_use]
    #[inline]
    pub const fn fg_mode(&self) -> u32 {
        (self.0 >> Self::FG_MODE_SHIFT) & Self::MODE_MASK
    }

    /// Get background color mode.
    #[must_use]
    #[inline]
    pub const fn bg_mode(&self) -> u32 {
        (self.0 >> Self::BG_MODE_SHIFT) & Self::MODE_MASK
    }

    /// Check if foreground is default.
    #[must_use]
    #[inline]
    pub const fn fg_is_default(&self) -> bool {
        self.fg_mode() == Self::MODE_DEFAULT
    }

    /// Check if background is default.
    #[must_use]
    #[inline]
    pub const fn bg_is_default(&self) -> bool {
        self.bg_mode() == Self::MODE_DEFAULT
    }

    /// Check if foreground is indexed.
    #[must_use]
    #[inline]
    pub const fn fg_is_indexed(&self) -> bool {
        self.fg_mode() == Self::MODE_INDEXED
    }

    /// Check if background is indexed.
    #[must_use]
    #[inline]
    pub const fn bg_is_indexed(&self) -> bool {
        self.bg_mode() == Self::MODE_INDEXED
    }

    /// Check if foreground is RGB (needs overflow lookup).
    #[must_use]
    #[inline]
    pub const fn fg_is_rgb(&self) -> bool {
        self.fg_mode() == Self::MODE_RGB
    }

    /// Check if background is RGB (needs overflow lookup).
    #[must_use]
    #[inline]
    pub const fn bg_is_rgb(&self) -> bool {
        self.bg_mode() == Self::MODE_RGB
    }

    /// Get foreground indexed color (only valid if `fg_is_indexed()`).
    #[must_use]
    #[inline]
    pub const fn fg_index(&self) -> u8 {
        (self.0 & 0xFF) as u8
    }

    /// Get background indexed color (only valid if `bg_is_indexed()`).
    #[must_use]
    #[inline]
    pub const fn bg_index(&self) -> u8 {
        ((self.0 >> 8) & 0xFF) as u8
    }

    /// Set foreground to indexed color.
    #[must_use]
    #[inline]
    pub const fn set_fg_indexed(self, index: u8) -> Self {
        Self(
            (self.0 & !0xFF & !(Self::MODE_MASK << Self::FG_MODE_SHIFT))
                | (index as u32)
                | (Self::MODE_INDEXED << Self::FG_MODE_SHIFT),
        )
    }

    /// Set background to indexed color.
    #[must_use]
    #[inline]
    pub const fn set_bg_indexed(self, index: u8) -> Self {
        Self(
            (self.0 & !0xFF00 & !(Self::MODE_MASK << Self::BG_MODE_SHIFT))
                | ((index as u32) << 8)
                | (Self::MODE_INDEXED << Self::BG_MODE_SHIFT),
        )
    }

    /// Set foreground to default.
    #[must_use]
    #[inline]
    pub const fn set_fg_default(self) -> Self {
        Self(self.0 & !(Self::MODE_MASK << Self::FG_MODE_SHIFT))
    }

    /// Set background to default.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub const fn set_bg_default(self) -> Self {
        Self(self.0 & !(Self::MODE_MASK << Self::BG_MODE_SHIFT))
    }

    /// Check if both colors are default.
    #[must_use]
    #[inline]
    pub const fn is_default(&self) -> bool {
        self.fg_is_default() && self.bg_is_default()
    }

    const HAS_EXTRAS_BIT: u32 = 1 << 16;

    /// Check if this cell has a CellExtras entry.
    #[must_use]
    #[inline]
    pub const fn has_extras(&self) -> bool {
        (self.0 & Self::HAS_EXTRAS_BIT) != 0
    }

    /// Set the HAS_EXTRAS flag.
    #[must_use]
    #[inline]
    pub const fn with_extras_flag(self) -> Self {
        Self(self.0 | Self::HAS_EXTRAS_BIT)
    }

    /// Clear the HAS_EXTRAS flag.
    #[must_use]
    #[inline]
    pub const fn without_extras_flag(self) -> Self {
        Self(self.0 & !Self::HAS_EXTRAS_BIT)
    }
}

/// Single-colour packed form, `0xTT_RRGGBB` where TT is the type:
/// - `0x00_0000II`: indexed colour (0-255)
/// - `0x01_RRGGBB`: truecolor RGB
/// - `0xFF_______`: default colour
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct PackedColor(pub u32);

impl PackedColor {
    /// Default foreground color.
    pub const DEFAULT_FG: Self = Self(0xFF_FFFFFF);

    /// Default background color.
    pub const DEFAULT_BG: Self = Self(0xFF_000000);

    /// Create an indexed color (0-255).
    #[must_use]
    #[inline]
    pub const fn indexed(index: u8) -> Self {
        Self(index as u32)
    }

    /// Create a true color from RGB values.
    #[must_use]
    #[inline]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self(0x01_000000 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32))
    }

    /// Check if this is the default color.
    #[must_use]
    #[inline]
    pub const fn is_default(&self) -> bool {
        (self.0 >> 24) == 0xFF
    }

    /// Check if this is an indexed color.
    #[must_use]
    #[inline]
    pub const fn is_indexed(&self) -> bool {
        (self.0 >> 24) == 0x00
    }

    /// Check if this is a true color.
    #[must_use]
    #[inline]
    pub const fn is_rgb(&self) -> bool {
        (self.0 >> 24) == 0x01
    }

    /// Get the indexed color value (only valid if `is_indexed()`).
    #[must_use]
    #[inline]
    pub const fn index(&self) -> u8 {
        (self.0 & 0xFF) as u8
    }

    /// Get RGB components (only valid if `is_rgb()`).
    #[must_use]
    #[inline]
    pub const fn rgb_components(&self) -> (u8, u8, u8) {
        let r = ((self.0 >> 16) & 0xFF) as u8;
        let g = ((self.0 >> 8) & 0xFF) as u8;
        let b = (self.0 & 0xFF) as u8;
        (r, g, b)
    }
}

#[cfg(test)]
#[path = "cell_colors_tests.rs"]
mod tests;
