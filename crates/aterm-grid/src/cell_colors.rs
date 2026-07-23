// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Packed color types for 8-byte terminal cells.

/// Packed color representation for 8-byte cells.
///
/// Encodes both foreground and background in 4 bytes.
///
/// ## Color Modes (bits 24-27 for FG, bits 28-31 for BG)
/// - 0x0: Default color
/// - 0x1: Indexed color (index in low bits)
/// - 0x2: RGB color (lookup in overflow table)
///
/// ## Indexed Colors (when mode = 0x1)
/// - FG index in bits 0-7
/// - BG index in bits 8-15
///
/// ## RGB Colors (when mode = 0x2)
/// Colors are stored in CellExtra overflow tables, keyed by (row, col).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct PackedColors(pub u32);

impl PackedColors {
    // Color mode flags (4 bits each for FG and BG)
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
    pub const fn new() -> Self {
        Self::DEFAULT
    }

    /// Create with indexed foreground and default background.
    #[must_use]
    #[inline]
    pub const fn with_indexed_fg(fg_index: u8) -> Self {
        Self((Self::MODE_INDEXED << Self::FG_MODE_SHIFT) | (fg_index as u32))
    }

    /// Create with indexed background and default foreground.
    #[must_use]
    #[inline]
    pub const fn with_indexed_bg(bg_index: u8) -> Self {
        Self((Self::MODE_INDEXED << Self::BG_MODE_SHIFT) | ((bg_index as u32) << 8))
    }

    /// Create with both indexed colors.
    #[must_use]
    #[inline]
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
    pub const fn set_bg_default(self) -> Self {
        Self(self.0 & !(Self::MODE_MASK << Self::BG_MODE_SHIFT))
    }

    /// Check if both colors are default.
    #[must_use]
    #[inline]
    pub const fn is_default(&self) -> bool {
        self.fg_is_default() && self.bg_is_default()
    }

    // --- Per-cell HAS_EXTRAS flag (bit 16) ---
    // Indicates this cell has an entry in the CellExtras HashMap.
    // Eliminates hash probes for cells without extras in the rendering path.

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

/// Legacy PackedColor for compatibility during transition.
///
/// Format: `0xTT_RRGGBB` where TT is the type:
/// - `0x00_INDEX__`: Indexed color (0-255)
/// - `0x01_RRGGBB`: True color RGB
/// - `0xFF_______`: Default color
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

    /// Get the raw packed u32 value.
    #[must_use]
    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- PackedColors: default state ----

    #[test]
    fn packed_colors_default_is_all_default() {
        let c = PackedColors::DEFAULT;
        assert!(c.fg_is_default());
        assert!(c.bg_is_default());
        assert!(c.is_default());
        assert_eq!(c.0, 0);
    }

    #[test]
    fn packed_colors_new_equals_default() {
        assert_eq!(PackedColors::new(), PackedColors::DEFAULT);
    }

    #[test]
    fn packed_colors_default_trait_equals_const_default() {
        assert_eq!(PackedColors::default(), PackedColors::DEFAULT);
    }

    // ---- PackedColors: indexed FG ----

    #[test]
    fn packed_colors_with_indexed_fg_mode() {
        let c = PackedColors::with_indexed_fg(42);
        assert!(c.fg_is_indexed());
        assert!(c.bg_is_default());
        assert_eq!(c.fg_index(), 42);
    }

    #[test]
    fn packed_colors_with_indexed_fg_zero() {
        let c = PackedColors::with_indexed_fg(0);
        assert!(c.fg_is_indexed());
        assert_eq!(c.fg_index(), 0);
    }

    #[test]
    fn packed_colors_with_indexed_fg_max() {
        let c = PackedColors::with_indexed_fg(255);
        assert!(c.fg_is_indexed());
        assert_eq!(c.fg_index(), 255);
    }

    // ---- PackedColors: indexed BG ----

    #[test]
    fn packed_colors_with_indexed_bg_mode() {
        let c = PackedColors::with_indexed_bg(99);
        assert!(c.bg_is_indexed());
        assert!(c.fg_is_default());
        assert_eq!(c.bg_index(), 99);
    }

    #[test]
    fn packed_colors_with_indexed_bg_zero() {
        let c = PackedColors::with_indexed_bg(0);
        assert!(c.bg_is_indexed());
        assert_eq!(c.bg_index(), 0);
    }

    #[test]
    fn packed_colors_with_indexed_bg_max() {
        let c = PackedColors::with_indexed_bg(255);
        assert!(c.bg_is_indexed());
        assert_eq!(c.bg_index(), 255);
    }

    // ---- PackedColors: both indexed ----

    #[test]
    fn packed_colors_with_indexed_both() {
        let c = PackedColors::with_indexed(196, 21);
        assert!(c.fg_is_indexed());
        assert!(c.bg_is_indexed());
        assert_eq!(c.fg_index(), 196);
        assert_eq!(c.bg_index(), 21);
        assert!(!c.is_default());
    }

    #[test]
    fn packed_colors_with_indexed_both_boundary() {
        let c = PackedColors::with_indexed(0, 255);
        assert_eq!(c.fg_index(), 0);
        assert_eq!(c.bg_index(), 255);

        let c2 = PackedColors::with_indexed(255, 0);
        assert_eq!(c2.fg_index(), 255);
        assert_eq!(c2.bg_index(), 0);
    }

    // ---- PackedColors: RGB markers ----

    #[test]
    fn packed_colors_with_rgb_fg_marker() {
        let c = PackedColors::new().with_rgb_fg();
        assert!(c.fg_is_rgb());
        assert!(c.bg_is_default());
        assert!(!c.fg_is_default());
        assert!(!c.fg_is_indexed());
    }

    #[test]
    fn packed_colors_with_rgb_bg_marker() {
        let c = PackedColors::new().with_rgb_bg();
        assert!(c.bg_is_rgb());
        assert!(c.fg_is_default());
        assert!(!c.bg_is_default());
        assert!(!c.bg_is_indexed());
    }

    #[test]
    fn packed_colors_both_rgb() {
        let c = PackedColors::new().with_rgb_fg().with_rgb_bg();
        assert!(c.fg_is_rgb());
        assert!(c.bg_is_rgb());
        assert!(!c.is_default());
    }

    // ---- PackedColors: mutually exclusive states ----

    #[test]
    fn packed_colors_set_fg_indexed_clears_rgb() {
        let c = PackedColors::new().with_rgb_fg().set_fg_indexed(77);
        assert!(c.fg_is_indexed());
        assert!(!c.fg_is_rgb());
        assert_eq!(c.fg_index(), 77);
    }

    #[test]
    fn packed_colors_set_bg_indexed_clears_rgb() {
        let c = PackedColors::new().with_rgb_bg().set_bg_indexed(33);
        assert!(c.bg_is_indexed());
        assert!(!c.bg_is_rgb());
        assert_eq!(c.bg_index(), 33);
    }

    #[test]
    fn packed_colors_set_fg_default_clears_indexed() {
        let c = PackedColors::with_indexed_fg(42).set_fg_default();
        assert!(c.fg_is_default());
        assert!(!c.fg_is_indexed());
    }

    #[test]
    fn packed_colors_set_bg_default_clears_indexed() {
        let c = PackedColors::with_indexed_bg(42).set_bg_default();
        assert!(c.bg_is_default());
        assert!(!c.bg_is_indexed());
    }

    #[test]
    fn packed_colors_rgb_fg_overwrites_indexed_fg() {
        let c = PackedColors::with_indexed_fg(100).with_rgb_fg();
        assert!(c.fg_is_rgb());
        assert!(!c.fg_is_indexed());
    }

    // ---- PackedColors: round-trip ----

    #[test]
    fn packed_colors_fg_index_roundtrip() {
        for idx in [0u8, 1, 15, 128, 254, 255] {
            let c = PackedColors::with_indexed_fg(idx);
            assert_eq!(c.fg_index(), idx, "fg round-trip failed for index {idx}");
        }
    }

    #[test]
    fn packed_colors_bg_index_roundtrip() {
        for idx in [0u8, 1, 15, 128, 254, 255] {
            let c = PackedColors::with_indexed_bg(idx);
            assert_eq!(c.bg_index(), idx, "bg round-trip failed for index {idx}");
        }
    }

    #[test]
    fn packed_colors_set_fg_indexed_roundtrip() {
        let c = PackedColors::new().set_fg_indexed(200);
        assert_eq!(c.fg_index(), 200);
        let c2 = c.set_fg_indexed(50);
        assert_eq!(c2.fg_index(), 50);
    }

    #[test]
    fn packed_colors_set_bg_indexed_roundtrip() {
        let c = PackedColors::new().set_bg_indexed(200);
        assert_eq!(c.bg_index(), 200);
        let c2 = c.set_bg_indexed(50);
        assert_eq!(c2.bg_index(), 50);
    }

    // ---- PackedColors: independence of FG and BG ----

    #[test]
    fn packed_colors_set_fg_preserves_bg() {
        let c = PackedColors::with_indexed_bg(88).set_fg_indexed(11);
        assert_eq!(c.bg_index(), 88);
        assert!(c.bg_is_indexed());
        assert_eq!(c.fg_index(), 11);
    }

    #[test]
    fn packed_colors_set_bg_preserves_fg() {
        let c = PackedColors::with_indexed_fg(11).set_bg_indexed(88);
        assert_eq!(c.fg_index(), 11);
        assert!(c.fg_is_indexed());
        assert_eq!(c.bg_index(), 88);
    }

    #[test]
    fn packed_colors_set_fg_default_preserves_bg() {
        let c = PackedColors::with_indexed(100, 200).set_fg_default();
        assert!(c.fg_is_default());
        assert!(c.bg_is_indexed());
        assert_eq!(c.bg_index(), 200);
    }

    #[test]
    fn packed_colors_set_bg_default_preserves_fg() {
        let c = PackedColors::with_indexed(100, 200).set_bg_default();
        assert!(c.bg_is_default());
        assert!(c.fg_is_indexed());
        assert_eq!(c.fg_index(), 100);
    }

    // ---- PackedColors: HAS_EXTRAS flag ----

    #[test]
    fn packed_colors_extras_flag_default_off() {
        assert!(!PackedColors::DEFAULT.has_extras());
    }

    #[test]
    fn packed_colors_extras_flag_set_and_clear() {
        let c = PackedColors::DEFAULT.with_extras_flag();
        assert!(c.has_extras());
        let c2 = c.without_extras_flag();
        assert!(!c2.has_extras());
    }

    #[test]
    fn packed_colors_extras_flag_preserves_colors() {
        let c = PackedColors::with_indexed(42, 99).with_extras_flag();
        assert!(c.has_extras());
        assert_eq!(c.fg_index(), 42);
        assert_eq!(c.bg_index(), 99);
        assert!(c.fg_is_indexed());
        assert!(c.bg_is_indexed());
    }

    // ---- PackedColor (legacy): default ----

    #[test]
    fn packed_color_default_fg_is_default() {
        assert!(PackedColor::DEFAULT_FG.is_default());
        assert!(!PackedColor::DEFAULT_FG.is_indexed());
        assert!(!PackedColor::DEFAULT_FG.is_rgb());
    }

    #[test]
    fn packed_color_default_bg_is_default() {
        assert!(PackedColor::DEFAULT_BG.is_default());
        assert!(!PackedColor::DEFAULT_BG.is_indexed());
        assert!(!PackedColor::DEFAULT_BG.is_rgb());
    }

    // ---- PackedColor (legacy): indexed ----

    #[test]
    fn packed_color_indexed_zero() {
        let c = PackedColor::indexed(0);
        assert!(c.is_indexed());
        assert!(!c.is_default());
        assert!(!c.is_rgb());
        assert_eq!(c.index(), 0);
    }

    #[test]
    fn packed_color_indexed_max() {
        let c = PackedColor::indexed(255);
        assert!(c.is_indexed());
        assert_eq!(c.index(), 255);
    }

    #[test]
    fn packed_color_indexed_roundtrip() {
        for idx in [0u8, 1, 7, 15, 127, 128, 254, 255] {
            let c = PackedColor::indexed(idx);
            assert_eq!(c.index(), idx, "indexed round-trip failed for {idx}");
        }
    }

    // ---- PackedColor (legacy): RGB ----

    #[test]
    fn packed_color_rgb_mode() {
        let c = PackedColor::rgb(255, 128, 0);
        assert!(c.is_rgb());
        assert!(!c.is_default());
        assert!(!c.is_indexed());
    }

    #[test]
    fn packed_color_rgb_roundtrip() {
        let c = PackedColor::rgb(10, 20, 30);
        assert_eq!(c.rgb_components(), (10, 20, 30));
    }

    #[test]
    fn packed_color_rgb_boundary_values() {
        let c = PackedColor::rgb(0, 0, 0);
        assert_eq!(c.rgb_components(), (0, 0, 0));
        assert!(c.is_rgb());

        let c = PackedColor::rgb(255, 255, 255);
        assert_eq!(c.rgb_components(), (255, 255, 255));
        assert!(c.is_rgb());
    }

    #[test]
    fn packed_color_rgb_raw_encoding() {
        let c = PackedColor::rgb(0xAB, 0xCD, 0xEF);
        assert_eq!(c.raw(), 0x01_ABCDEF);
    }

    #[test]
    fn packed_color_indexed_raw_encoding() {
        let c = PackedColor::indexed(42);
        assert_eq!(c.raw(), 42);
    }
}
