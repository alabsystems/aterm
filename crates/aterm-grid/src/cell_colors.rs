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

    /// One side's colour mode, for the exhaustive walks below.
    #[derive(Clone, Copy, Debug)]
    enum Side {
        Default,
        Indexed(u8),
        Rgb,
    }

    /// Every state a side can be in: default, RGB and all 256 indexes.
    fn every_side() -> impl Iterator<Item = Side> {
        [Side::Default, Side::Rgb]
            .into_iter()
            .chain((0..=255u8).map(Side::Indexed))
    }

    /// Assert one side reads back as `want` and as nothing else.
    fn assert_side(
        label: &str,
        (default, indexed, rgb, index): (bool, bool, bool, u8),
        want: Side,
    ) {
        match want {
            Side::Default => assert!(default && !indexed && !rgb, "{label}: want default"),
            Side::Indexed(i) => {
                assert!(!default && indexed && !rgb, "{label}: want indexed");
                assert_eq!(index, i, "{label}: index");
            }
            Side::Rgb => assert!(!default && !indexed && rgb, "{label}: want rgb"),
        }
    }

    fn fg(c: PackedColors) -> (bool, bool, bool, u8) {
        (
            c.fg_is_default(),
            c.fg_is_indexed(),
            c.fg_is_rgb(),
            c.fg_index(),
        )
    }

    fn bg(c: PackedColors) -> (bool, bool, bool, u8) {
        (
            c.bg_is_default(),
            c.bg_is_indexed(),
            c.bg_is_rgb(),
            c.bg_index(),
        )
    }

    fn set_fg(c: PackedColors, side: Side) -> PackedColors {
        match side {
            Side::Default => c.set_fg_default(),
            Side::Indexed(i) => c.set_fg_indexed(i),
            Side::Rgb => c.with_rgb_fg(),
        }
    }

    fn set_bg(c: PackedColors, side: Side) -> PackedColors {
        match side {
            Side::Default => c.set_bg_default(),
            Side::Indexed(i) => c.set_bg_indexed(i),
            Side::Rgb => c.with_rgb_bg(),
        }
    }

    #[test]
    fn packed_colors_every_fg_bg_and_extras_state() {
        // One walk in place of 30 one-state tests here and 6 twins in
        // cell_tests.rs: the default value, indexed fg/bg/both at 0, mid and 255,
        // the RGB markers, a mode switch clearing the previous mode, index round
        // trips, fg/bg independence, and the HAS_EXTRAS bit leaving the colours
        // alone.
        assert_eq!(PackedColors::DEFAULT.0, 0);
        assert_eq!(PackedColors::new(), PackedColors::DEFAULT);
        assert_eq!(PackedColors::default(), PackedColors::DEFAULT);
        assert!(PackedColors::DEFAULT.is_default());
        assert!(!PackedColors::DEFAULT.has_extras());

        for i in 0..=255u8 {
            let only_fg = PackedColors::with_indexed_fg(i);
            assert_side("with_indexed_fg fg", fg(only_fg), Side::Indexed(i));
            assert_side("with_indexed_fg bg", bg(only_fg), Side::Default);
            let only_bg = PackedColors::with_indexed_bg(i);
            assert_side("with_indexed_bg fg", fg(only_bg), Side::Default);
            assert_side("with_indexed_bg bg", bg(only_bg), Side::Indexed(i));
            for j in 0..=255u8 {
                let both = PackedColors::with_indexed(i, j);
                assert_side("with_indexed fg", fg(both), Side::Indexed(i));
                assert_side("with_indexed bg", bg(both), Side::Indexed(j));
                assert!(!both.is_default() && !both.has_extras());
            }
        }

        // Every (fg, bg) state reached by the setters from every kind of starting
        // value, in both orders, so each setter must clear the mode it replaces
        // and leave the other side untouched.
        let starts = [
            PackedColors::DEFAULT,
            PackedColors::with_indexed(100, 200),
            PackedColors::DEFAULT.with_rgb_fg().with_rgb_bg(),
            PackedColors::with_indexed(7, 9).with_extras_flag(),
        ];
        for start in starts {
            for want_fg in every_side() {
                for want_bg in every_side() {
                    let fg_first = set_bg(set_fg(start, want_fg), want_bg);
                    let bg_first = set_fg(set_bg(start, want_bg), want_fg);
                    for c in [fg_first, bg_first] {
                        for extras in [false, true] {
                            let c = if extras {
                                c.with_extras_flag()
                            } else {
                                c.without_extras_flag()
                            };
                            assert_eq!(c.has_extras(), extras, "{start:?} -> {c:?}");
                            assert_side("fg", fg(c), want_fg);
                            assert_side("bg", bg(c), want_bg);
                            assert_eq!(
                                c.is_default(),
                                matches!((want_fg, want_bg), (Side::Default, Side::Default)),
                                "{c:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn packed_color_every_mode() {
        // One walk in place of 8 one-value tests here and 3 twins in
        // cell_tests.rs. #6704: DEFAULT_FG/BG must carry type byte 0xFF
        // ("default"), never 0x00 ("indexed"); zero-initialised cells with an
        // indexed-black background drew dark bands on light themes.
        for d in [PackedColor::DEFAULT_FG, PackedColor::DEFAULT_BG] {
            assert!(d.is_default(), "{d:?}");
            assert!(!d.is_indexed(), "{d:?}");
            assert!(!d.is_rgb(), "{d:?}");
        }
        for i in 0..=255u8 {
            let c = PackedColor::indexed(i);
            assert!(c.is_indexed() && !c.is_default() && !c.is_rgb(), "{c:?}");
            assert_eq!(c.index(), i);
        }
        let components = [0u8, 1, 10, 20, 30, 64, 127, 128, 254, 255];
        for r in components {
            for g in components {
                for b in components {
                    let c = PackedColor::rgb(r, g, b);
                    assert!(c.is_rgb() && !c.is_default() && !c.is_indexed(), "{c:?}");
                    assert_eq!(c.rgb_components(), (r, g, b));
                }
            }
        }
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
