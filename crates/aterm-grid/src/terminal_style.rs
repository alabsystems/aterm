// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal style types shared between grid and checkpoint systems.
//!
//! These types represent terminal rendering state (current style attributes,
//! saved cursor state) that both the grid cell model and checkpoint
//! serialization need access to. Placed in aterm-grid because they depend
//! on grid primitives (`PackedColor`, `CellFlags`, `Cursor`).

use crate::cell::{CellFlags, PackedColor};
use crate::cell_colors::PackedColors;
use crate::cursor::Cursor;
use crate::style::{Color, ColorType, ExtendedStyle, Style, StyleAttrs};
use aterm_types::charset::CharacterSetState;

/// Current style attributes for new characters.
#[derive(Debug, Clone, Copy)]
pub struct CurrentStyle {
    /// Foreground color.
    pub fg: PackedColor,
    /// Background color.
    pub bg: PackedColor,
    /// Cell flags (bold, italic, etc.).
    pub flags: CellFlags,
    /// Whether characters are protected from selective erase (DECSCA).
    /// When false (default), characters can be erased by DECSED/DECSEL.
    /// When true, characters are protected and selective erase skips them.
    pub protected: bool,
    /// Cached packed colors — avoids `convert_colors` per character write.
    /// Updated on every SGR change via `update_cached_colors()`.
    cached_colors: PackedColors,
    /// Cached flag: true when style alone requires CellExtras overflow.
    /// Covers `flags.has_extended_flags() || fg.is_rgb() || bg.is_rgb()`.
    /// Updated alongside `cached_colors` to avoid 3 per-character checks.
    cached_has_style_extras: bool,
    /// Cached flag: true when style is completely default (no colors, no flags).
    /// When true, `write_ascii_blast` can be used instead of styled writes.
    /// Updated alongside `cached_colors` to avoid 4 per-bulk-call checks.
    cached_is_default: bool,
    /// Cached StyleAttrs derived from CellFlags.
    /// Avoids the 12-iteration `cell_flags_to_attrs` loop on color-only SGR changes.
    /// Updated when flags change via `update_cached_colors()`.
    cached_attrs: StyleAttrs,
    /// Cached fg Color for style intern.
    /// Avoids `unpack_color` branch chain on every `update_style_id`.
    cached_fg_color: Color,
    /// Cached bg Color for style intern.
    cached_bg_color: Color,
    /// Cached fg `ColorType` and palette index for style intern.
    /// Avoids redundant `packed_color_type` branch chain on color-only SGR changes.
    cached_fg_type: ColorType,
    cached_fg_index: u8,
    /// Cached bg `ColorType` and palette index for style intern.
    cached_bg_type: ColorType,
    cached_bg_index: u8,
}

impl Default for CurrentStyle {
    fn default() -> Self {
        let fg = PackedColor::DEFAULT_FG;
        let bg = PackedColor::DEFAULT_BG;
        Self {
            fg,
            bg,
            flags: CellFlags::empty(),
            protected: false,
            cached_colors: crate::Cell::convert_colors(fg, bg),
            cached_has_style_extras: false,
            cached_is_default: true,
            cached_attrs: StyleAttrs::empty(),
            cached_fg_color: Color::DEFAULT_FG,
            cached_bg_color: Color::DEFAULT_BG,
            cached_fg_type: ColorType::Default,
            cached_fg_index: 0,
            cached_bg_type: ColorType::Default,
            cached_bg_index: 0,
        }
    }
}

impl CurrentStyle {
    /// Construct from explicit fields (checkpoint deserialization).
    #[must_use]
    pub fn new(fg: PackedColor, bg: PackedColor, flags: CellFlags, protected: bool) -> Self {
        let ext = ExtendedStyle::from_packed_colors_separate(fg, bg, flags);
        let (fg_type, fg_index) = Self::packed_color_type(fg);
        let (bg_type, bg_index) = Self::packed_color_type(bg);
        Self {
            fg,
            bg,
            flags,
            protected,
            cached_colors: crate::Cell::convert_colors(fg, bg),
            cached_has_style_extras: flags.has_extended_flags() || fg.is_rgb() || bg.is_rgb(),
            cached_is_default: fg.is_default() && bg.is_default() && flags.is_empty() && !protected,
            cached_attrs: ext.style.attrs,
            cached_fg_color: ext.style.fg,
            cached_bg_color: ext.style.bg,
            cached_fg_type: fg_type,
            cached_fg_index: fg_index,
            cached_bg_type: bg_type,
            cached_bg_index: bg_index,
        }
    }

    /// Foreground color as packed u32.
    #[must_use]
    pub fn fg_packed(&self) -> u32 {
        self.fg.0
    }

    /// Background color as packed u32.
    #[must_use]
    pub fn bg_packed(&self) -> u32 {
        self.bg.0
    }

    /// Cell flags as raw u16 bits.
    #[must_use]
    pub fn flags_bits(&self) -> u16 {
        self.flags.bits()
    }

    /// Cached packed colors for writing cells without per-character conversion.
    #[must_use]
    #[inline]
    pub fn cached_colors(&self) -> PackedColors {
        self.cached_colors
    }

    /// Extract the background RGB value for BCE, if the background is truecolor.
    ///
    /// Returns `Some([r, g, b])` when `self.bg.is_rgb()`, `None` otherwise.
    /// Used by `set_cursor_template` callers to propagate truecolor backgrounds
    /// through erase/fill/scroll operations (#7685).
    #[must_use]
    #[inline]
    pub fn bce_bg_rgb(&self) -> Option<[u8; 3]> {
        if self.bg.is_rgb() {
            let (r, g, b) = self.bg.rgb_components();
            Some([r, g, b])
        } else {
            None
        }
    }

    /// Whether the style alone requires CellExtras overflow.
    #[must_use]
    #[inline]
    pub fn has_style_extras(&self) -> bool {
        self.cached_has_style_extras
    }

    /// Whether the style is completely default (no colors, no flags, not protected).
    ///
    /// When true, the ultra-fast `write_ascii_blast` path can be used.
    #[must_use]
    #[inline]
    pub fn is_default(&self) -> bool {
        self.cached_is_default
    }

    /// Recompute all cached state from current fg/bg/flags/protected.
    ///
    /// Must be called after any change to `fg`, `bg`, `flags`, or `protected`.
    #[inline]
    pub fn update_cached_colors(&mut self) {
        self.cached_colors = crate::Cell::convert_colors(self.fg, self.bg);
        self.cached_has_style_extras =
            self.flags.has_extended_flags() || self.fg.is_rgb() || self.bg.is_rgb();
        self.cached_is_default = self.fg.is_default()
            && self.bg.is_default()
            && self.flags.is_empty()
            && !self.protected;
        // Update style intern cache: attrs from flags, colors from packed.
        self.cached_attrs = ExtendedStyle::cell_flags_to_style_attrs(self.flags);
        self.cached_fg_color = Self::packed_to_color(self.fg, Color::DEFAULT_FG);
        self.cached_bg_color = Self::packed_to_color(self.bg, Color::DEFAULT_BG);
        let (fg_type, fg_index) = Self::packed_color_type(self.fg);
        let (bg_type, bg_index) = Self::packed_color_type(self.bg);
        self.cached_fg_type = fg_type;
        self.cached_fg_index = fg_index;
        self.cached_bg_type = bg_type;
        self.cached_bg_index = bg_index;
    }

    /// Recompute only the flag-derived caches after a flags-only SGR change.
    ///
    /// A flags-only SGR (e.g. `\x1b[1m`, `\x1b[7m`, `\x1b[22m`) flips attribute
    /// bits without touching `fg`/`bg`. The cached color fields
    /// (`cached_fg_color`, `cached_bg_color`, `cached_colors`, color types and
    /// indices) therefore remain valid and are deliberately NOT recomputed —
    /// only `cached_attrs` and the two flag-dependent booleans need refreshing.
    /// This is the allocation-free fast path that avoids `convert_colors`,
    /// `packed_to_color` and `packed_color_type` on every attribute toggle.
    #[inline]
    pub fn update_flags_cache(&mut self) {
        self.cached_attrs = ExtendedStyle::cell_flags_to_style_attrs(self.flags);
        self.cached_has_style_extras =
            self.flags.has_extended_flags() || self.fg.is_rgb() || self.bg.is_rgb();
        self.cached_is_default = self.fg.is_default()
            && self.bg.is_default()
            && self.flags.is_empty()
            && !self.protected;
    }

    /// Build just the `Style` (fg, bg, attrs) from cached fields.
    ///
    /// Cheaper than `build_extended_style` — omits `fg_type`/`bg_type`/index
    /// fields. Used for L1 cache probes where only the `Style` is needed for
    /// comparison before deciding whether to build the full `ExtendedStyle`.
    #[inline]
    #[must_use]
    pub fn build_style(&self) -> Style {
        Style {
            fg: self.cached_fg_color,
            bg: self.cached_bg_color,
            attrs: self.cached_attrs,
        }
    }

    /// Cached bg color for constructing L2 probe styles without full rebuild.
    #[inline]
    #[must_use]
    pub fn cached_bg_color(&self) -> Color {
        self.cached_bg_color
    }

    /// Cached style attrs for constructing L2 probe styles without full rebuild.
    #[inline]
    #[must_use]
    pub fn cached_attrs(&self) -> StyleAttrs {
        self.cached_attrs
    }

    /// Update only fg-related caches after an indexed fg color change.
    ///
    /// Called on L1/L2 cache hit to keep caches consistent without the full
    /// `build_extended_style_fg_changed` recomputation. Only updates fg color,
    /// fg type/index, packed colors, and derived flags.
    #[inline]
    pub fn update_fg_cache_indexed(&mut self, index: u8) {
        self.cached_fg_color = Color::from_ansi_256(index);
        self.cached_fg_type = ColorType::Indexed;
        self.cached_fg_index = index;
        self.cached_colors = crate::Cell::convert_colors(self.fg, self.bg);
        self.cached_is_default = false;
    }

    /// Build an `ExtendedStyle` for intern lookup using cached attrs/colors.
    ///
    /// This avoids the `cell_flags_to_attrs` loop (12 iterations) and the
    /// `unpack_color` branch chain when only the fg or bg color has changed.
    /// Call `update_cached_colors()` first to ensure caches are fresh.
    #[inline]
    #[must_use]
    pub fn build_extended_style(&self) -> ExtendedStyle {
        ExtendedStyle {
            style: Style {
                fg: self.cached_fg_color,
                bg: self.cached_bg_color,
                attrs: self.cached_attrs,
            },
            fg_type: self.cached_fg_type,
            bg_type: self.cached_bg_type,
            fg_index: self.cached_fg_index,
            bg_index: self.cached_bg_index,
        }
    }

    /// Build an `ExtendedStyle` after only the fg color changed.
    ///
    /// Updates cached fg color inline, reuses cached bg color and attrs.
    /// Avoids `update_cached_colors` + full `from_packed_colors_separate`.
    #[inline]
    #[must_use]
    pub fn build_extended_style_fg_changed(&mut self) -> ExtendedStyle {
        // Update only fg-related caches
        self.cached_fg_color = Self::packed_to_color(self.fg, Color::DEFAULT_FG);
        self.cached_colors = crate::Cell::convert_colors(self.fg, self.bg);
        self.cached_has_style_extras =
            self.flags.has_extended_flags() || self.fg.is_rgb() || self.bg.is_rgb();
        self.cached_is_default = self.fg.is_default()
            && self.bg.is_default()
            && self.flags.is_empty()
            && !self.protected;
        let (fg_type, fg_index) = Self::packed_color_type(self.fg);
        self.cached_fg_type = fg_type;
        self.cached_fg_index = fg_index;
        ExtendedStyle {
            style: Style {
                fg: self.cached_fg_color,
                bg: self.cached_bg_color,
                attrs: self.cached_attrs,
            },
            fg_type,
            bg_type: self.cached_bg_type,
            fg_index,
            bg_index: self.cached_bg_index,
        }
    }

    /// Build an `ExtendedStyle` after only the bg color changed.
    #[inline]
    #[must_use]
    pub fn build_extended_style_bg_changed(&mut self) -> ExtendedStyle {
        self.cached_bg_color = Self::packed_to_color(self.bg, Color::DEFAULT_BG);
        self.cached_colors = crate::Cell::convert_colors(self.fg, self.bg);
        self.cached_has_style_extras =
            self.flags.has_extended_flags() || self.fg.is_rgb() || self.bg.is_rgb();
        self.cached_is_default = self.fg.is_default()
            && self.bg.is_default()
            && self.flags.is_empty()
            && !self.protected;
        let (bg_type, bg_index) = Self::packed_color_type(self.bg);
        self.cached_bg_type = bg_type;
        self.cached_bg_index = bg_index;
        ExtendedStyle {
            style: Style {
                fg: self.cached_fg_color,
                bg: self.cached_bg_color,
                attrs: self.cached_attrs,
            },
            fg_type: self.cached_fg_type,
            bg_type,
            fg_index: self.cached_fg_index,
            bg_index,
        }
    }

    /// Build an `ExtendedStyle` after both fg and bg colors changed.
    #[inline]
    #[must_use]
    pub fn build_extended_style_both_changed(&mut self) -> ExtendedStyle {
        self.cached_fg_color = Self::packed_to_color(self.fg, Color::DEFAULT_FG);
        self.cached_bg_color = Self::packed_to_color(self.bg, Color::DEFAULT_BG);
        self.cached_colors = crate::Cell::convert_colors(self.fg, self.bg);
        self.cached_has_style_extras =
            self.flags.has_extended_flags() || self.fg.is_rgb() || self.bg.is_rgb();
        self.cached_is_default = self.fg.is_default()
            && self.bg.is_default()
            && self.flags.is_empty()
            && !self.protected;
        let (fg_type, fg_index) = Self::packed_color_type(self.fg);
        let (bg_type, bg_index) = Self::packed_color_type(self.bg);
        self.cached_fg_type = fg_type;
        self.cached_fg_index = fg_index;
        self.cached_bg_type = bg_type;
        self.cached_bg_index = bg_index;
        ExtendedStyle {
            style: Style {
                fg: self.cached_fg_color,
                bg: self.cached_bg_color,
                attrs: self.cached_attrs,
            },
            fg_type,
            bg_type,
            fg_index,
            bg_index,
        }
    }

    /// Convert a `PackedColor` to a `Color`.
    #[inline]
    fn packed_to_color(packed: PackedColor, default: Color) -> Color {
        if packed.is_default() {
            default
        } else if packed.is_indexed() {
            Color::from_ansi_256(packed.index())
        } else if packed.is_rgb() {
            let (r, g, b) = packed.rgb_components();
            Color::new(r, g, b)
        } else {
            default
        }
    }

    /// Get `ColorType` and index for a `PackedColor`.
    #[inline]
    fn packed_color_type(packed: PackedColor) -> (crate::style::ColorType, u8) {
        if packed.is_default() {
            (ColorType::Default, 0)
        } else if packed.is_indexed() {
            (ColorType::Indexed, packed.index())
        } else if packed.is_rgb() {
            (ColorType::Rgb, 0)
        } else {
            (ColorType::Default, 0)
        }
    }

    /// Reset to default style (full reset including DECSCA protection).
    ///
    /// Used by RIS (full terminal reset).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Reset SGR attributes only (colors and flags).
    ///
    /// This does NOT reset the DECSCA protection attribute, as per VT510 spec.
    /// SGR 0 resets character rendition attributes but not selective erase.
    pub fn reset_sgr(&mut self) {
        let protected = self.protected;
        *self = Self::default();
        self.protected = protected;
        // Recompute cached_is_default since protected may differ from default.
        self.cached_is_default = !protected;
    }
}

/// Saved cursor state for DECSC/DECRC.
///
/// Per VT510 specification, DECSC saves:
/// - Cursor position
/// - Character attributes (SGR)
/// - Character set (G0-G3, GL mapping, single shift)
/// - Wrap flag (DECAWM state)
/// - Origin mode (DECOM state)
/// - Selective erase attribute (DECSCA protection status)
#[derive(Debug, Clone, Copy, Default)]
pub struct SavedCursorState {
    /// Cursor position.
    pub cursor: Cursor,
    /// Saved text style (includes protection status via `protected` field).
    pub style: CurrentStyle,
    /// Origin mode at save time.
    pub origin_mode: bool,
    /// Auto-wrap mode at save time.
    pub auto_wrap: bool,
    /// Character set state (G0-G3, GL, single shift).
    pub charset: CharacterSetState,
    /// Pending wrap (wrap-next) flag at save time (#7283).
    pub pending_wrap: bool,
    /// Underline color at save time (SGR 58/59, #7295).
    ///
    /// Stored as packed u32 color (same encoding as `PackedColor`).
    /// `None` means default underline color (foreground).
    pub underline_color: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // CurrentStyle::default — empty/no attributes
    // =========================================================================

    #[test]
    fn test_default_fg_is_default() {
        let style = CurrentStyle::default();
        assert!(style.fg.is_default(), "default fg should be DEFAULT_FG");
    }

    #[test]
    fn test_default_bg_is_default() {
        let style = CurrentStyle::default();
        assert!(style.bg.is_default(), "default bg should be DEFAULT_BG");
    }

    #[test]
    fn test_default_flags_empty() {
        let style = CurrentStyle::default();
        assert!(style.flags.is_empty(), "default flags should be empty");
    }

    #[test]
    fn test_default_not_protected() {
        let style = CurrentStyle::default();
        assert!(!style.protected, "default should not be protected");
    }

    #[test]
    fn test_default_is_default_cached() {
        let style = CurrentStyle::default();
        assert!(
            style.is_default(),
            "default style should report is_default()"
        );
    }

    #[test]
    fn test_default_no_style_extras() {
        let style = CurrentStyle::default();
        assert!(
            !style.has_style_extras(),
            "default style should not need style extras"
        );
    }

    #[test]
    fn test_default_cached_attrs_empty() {
        let style = CurrentStyle::default();
        assert_eq!(
            style.cached_attrs,
            StyleAttrs::empty(),
            "default cached_attrs should be empty"
        );
    }

    // =========================================================================
    // Setting/clearing individual attributes
    // =========================================================================

    #[test]
    fn test_set_bold_flag() {
        let mut style = CurrentStyle::default();
        style.flags.insert(CellFlags::BOLD);
        style.update_cached_colors();
        assert!(style.flags.contains(CellFlags::BOLD));
        assert!(!style.is_default(), "bold style is not default");
    }

    #[test]
    fn test_set_italic_flag() {
        let mut style = CurrentStyle::default();
        style.flags.insert(CellFlags::ITALIC);
        style.update_cached_colors();
        assert!(style.flags.contains(CellFlags::ITALIC));
        assert!(!style.is_default());
    }

    #[test]
    fn test_set_underline_flag() {
        let mut style = CurrentStyle::default();
        style.flags.insert(CellFlags::UNDERLINE);
        style.update_cached_colors();
        assert!(style.flags.contains(CellFlags::UNDERLINE));
    }

    #[test]
    fn test_set_dim_flag() {
        let mut style = CurrentStyle::default();
        style.flags.insert(CellFlags::DIM);
        style.update_cached_colors();
        assert!(style.flags.contains(CellFlags::DIM));
        assert!(!style.is_default());
    }

    #[test]
    fn test_set_blink_flag() {
        let mut style = CurrentStyle::default();
        style.flags.insert(CellFlags::BLINK);
        style.update_cached_colors();
        assert!(style.flags.contains(CellFlags::BLINK));
    }

    #[test]
    fn test_set_inverse_flag() {
        let mut style = CurrentStyle::default();
        style.flags.insert(CellFlags::INVERSE);
        style.update_cached_colors();
        assert!(style.flags.contains(CellFlags::INVERSE));
    }

    #[test]
    fn test_set_hidden_flag() {
        let mut style = CurrentStyle::default();
        style.flags.insert(CellFlags::HIDDEN);
        style.update_cached_colors();
        assert!(style.flags.contains(CellFlags::HIDDEN));
    }

    #[test]
    fn test_set_strikethrough_flag() {
        let mut style = CurrentStyle::default();
        style.flags.insert(CellFlags::STRIKETHROUGH);
        style.update_cached_colors();
        assert!(style.flags.contains(CellFlags::STRIKETHROUGH));
    }

    #[test]
    fn test_set_overline_flag() {
        let mut style = CurrentStyle::default();
        style.flags.insert(CellFlags::OVERLINE);
        style.update_cached_colors();
        assert!(style.flags.contains(CellFlags::OVERLINE));
        assert!(
            style.has_style_extras(),
            "overline is an extended flag requiring style extras"
        );
    }

    #[test]
    fn test_clear_bold_flag() {
        let mut style = CurrentStyle::default();
        style.flags.insert(CellFlags::BOLD);
        style.flags.remove(CellFlags::BOLD);
        style.update_cached_colors();
        assert!(!style.flags.contains(CellFlags::BOLD));
        assert!(style.is_default(), "clearing bold should restore default");
    }

    #[test]
    fn test_multiple_flags_combined() {
        let mut style = CurrentStyle::default();
        style.flags.insert(CellFlags::BOLD);
        style.flags.insert(CellFlags::ITALIC);
        style.flags.insert(CellFlags::UNDERLINE);
        style.update_cached_colors();
        assert!(style.flags.contains(CellFlags::BOLD));
        assert!(style.flags.contains(CellFlags::ITALIC));
        assert!(style.flags.contains(CellFlags::UNDERLINE));
        assert!(!style.is_default());
    }

    // =========================================================================
    // Setting fg/bg colors (indexed, RGB)
    // =========================================================================

    #[test]
    fn test_set_indexed_fg_color() {
        let mut style = CurrentStyle {
            fg: PackedColor::indexed(1),
            ..CurrentStyle::default()
        };
        style.update_cached_colors();
        assert!(!style.is_default(), "indexed fg is not default");
        assert!(style.fg.is_indexed());
        assert_eq!(style.fg.index(), 1);
    }

    #[test]
    fn test_set_indexed_bg_color() {
        let mut style = CurrentStyle {
            bg: PackedColor::indexed(4),
            ..CurrentStyle::default()
        };
        style.update_cached_colors();
        assert!(!style.is_default(), "indexed bg is not default");
        assert!(style.bg.is_indexed());
        assert_eq!(style.bg.index(), 4);
    }

    #[test]
    fn test_set_rgb_fg_color() {
        let mut style = CurrentStyle {
            fg: PackedColor::rgb(255, 128, 0),
            ..CurrentStyle::default()
        };
        style.update_cached_colors();
        assert!(!style.is_default());
        assert!(style.fg.is_rgb());
        assert!(
            style.has_style_extras(),
            "RGB fg requires style extras overflow"
        );
        let (r, g, b) = style.fg.rgb_components();
        assert_eq!((r, g, b), (255, 128, 0));
    }

    #[test]
    fn test_set_rgb_bg_color() {
        let mut style = CurrentStyle {
            bg: PackedColor::rgb(0, 64, 128),
            ..CurrentStyle::default()
        };
        style.update_cached_colors();
        assert!(!style.is_default());
        assert!(style.bg.is_rgb());
        assert!(
            style.has_style_extras(),
            "RGB bg requires style extras overflow"
        );
    }

    #[test]
    fn test_both_rgb_colors() {
        let mut style = CurrentStyle {
            fg: PackedColor::rgb(10, 20, 30),
            bg: PackedColor::rgb(40, 50, 60),
            ..CurrentStyle::default()
        };
        style.update_cached_colors();
        assert!(style.has_style_extras());
        assert!(!style.is_default());
    }

    // =========================================================================
    // Style reset clears everything
    // =========================================================================

    #[test]
    fn test_reset_clears_all() {
        let mut style = CurrentStyle {
            fg: PackedColor::rgb(255, 0, 0),
            bg: PackedColor::indexed(5),
            ..CurrentStyle::default()
        };
        style.flags.insert(CellFlags::BOLD);
        style.flags.insert(CellFlags::ITALIC);
        style.protected = true;
        style.update_cached_colors();

        style.reset();

        assert!(style.fg.is_default());
        assert!(style.bg.is_default());
        assert!(style.flags.is_empty());
        assert!(!style.protected);
        assert!(style.is_default());
        assert!(!style.has_style_extras());
    }

    #[test]
    fn test_reset_sgr_preserves_protected() {
        let mut style = CurrentStyle {
            fg: PackedColor::rgb(255, 0, 0),
            ..CurrentStyle::default()
        };
        style.flags.insert(CellFlags::BOLD);
        style.protected = true;
        style.update_cached_colors();

        style.reset_sgr();

        assert!(style.fg.is_default(), "sgr reset should clear fg color");
        assert!(style.flags.is_empty(), "sgr reset should clear flags");
        assert!(style.protected, "sgr reset must NOT clear protected");
        assert!(
            !style.is_default(),
            "style with protected=true is not default"
        );
    }

    #[test]
    fn test_reset_sgr_without_protected_is_default() {
        let mut style = CurrentStyle {
            fg: PackedColor::indexed(3),
            ..CurrentStyle::default()
        };
        style.flags.insert(CellFlags::UNDERLINE);
        style.update_cached_colors();

        style.reset_sgr();

        assert!(
            style.is_default(),
            "sgr reset without protected should be default"
        );
    }

    // =========================================================================
    // Underline style variants
    // =========================================================================

    #[test]
    fn test_single_underline() {
        let mut style = CurrentStyle::default();
        style.flags.insert(CellFlags::UNDERLINE);
        style.update_cached_colors();
        assert!(style.flags.contains(CellFlags::UNDERLINE));
        assert!(!style.flags.contains(CellFlags::DOUBLE_UNDERLINE));
        assert!(!style.flags.contains(CellFlags::CURLY_UNDERLINE));
    }

    #[test]
    fn test_double_underline() {
        let mut style = CurrentStyle::default();
        style.flags.insert(CellFlags::DOUBLE_UNDERLINE);
        style.update_cached_colors();
        assert!(style.flags.contains(CellFlags::DOUBLE_UNDERLINE));
        assert!(!style.flags.contains(CellFlags::UNDERLINE));
    }

    #[test]
    fn test_curly_underline() {
        let mut style = CurrentStyle::default();
        style.flags.insert(CellFlags::CURLY_UNDERLINE);
        style.update_cached_colors();
        assert!(style.flags.contains(CellFlags::CURLY_UNDERLINE));
        assert!(
            style.has_style_extras(),
            "curly underline uses extended flag bits"
        );
    }

    #[test]
    fn test_dotted_underline() {
        let style = CurrentStyle::new(
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::DOTTED_UNDERLINE,
            false,
        );
        assert!(style.flags.contains(CellFlags::DOTTED_UNDERLINE));
        // Dotted = UNDERLINE | CURLY_UNDERLINE
        assert!(style.flags.contains(CellFlags::UNDERLINE));
        assert!(style.flags.contains(CellFlags::CURLY_UNDERLINE));
    }

    #[test]
    fn test_dashed_underline() {
        let style = CurrentStyle::new(
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::DASHED_UNDERLINE,
            false,
        );
        assert!(style.flags.contains(CellFlags::DASHED_UNDERLINE));
        // Dashed = DOUBLE_UNDERLINE | CURLY_UNDERLINE
        assert!(style.flags.contains(CellFlags::DOUBLE_UNDERLINE));
        assert!(style.flags.contains(CellFlags::CURLY_UNDERLINE));
    }

    // =========================================================================
    // Style comparison / equality
    // =========================================================================

    #[test]
    fn test_two_defaults_are_equal() {
        let a = CurrentStyle::default();
        let b = CurrentStyle::default();
        assert_eq!(a.fg, b.fg);
        assert_eq!(a.bg, b.bg);
        assert_eq!(a.flags, b.flags);
        assert_eq!(a.protected, b.protected);
    }

    #[test]
    fn test_same_style_new_vs_default() {
        let from_new = CurrentStyle::new(
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
            false,
        );
        let from_default = CurrentStyle::default();
        assert_eq!(from_new.fg, from_default.fg);
        assert_eq!(from_new.bg, from_default.bg);
        assert_eq!(from_new.flags, from_default.flags);
        assert_eq!(from_new.is_default(), from_default.is_default());
    }

    #[test]
    fn test_different_fg_not_equal() {
        let a = CurrentStyle::default();
        let b = CurrentStyle::new(
            PackedColor::indexed(1),
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
            false,
        );
        assert_ne!(a.fg, b.fg);
    }

    #[test]
    fn test_different_flags_not_equal() {
        let a = CurrentStyle::new(
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::BOLD,
            false,
        );
        let b = CurrentStyle::new(
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::ITALIC,
            false,
        );
        assert_ne!(a.flags, b.flags);
    }

    // =========================================================================
    // build_style / build_extended_style correctness
    // =========================================================================

    #[test]
    fn test_build_style_default() {
        let style = CurrentStyle::default();
        let built = style.build_style();
        assert_eq!(built, Style::DEFAULT);
    }

    #[test]
    fn test_build_style_with_indexed_fg() {
        let style = CurrentStyle::new(
            PackedColor::indexed(1),
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
            false,
        );
        let built = style.build_style();
        assert_eq!(built.fg, Color::from_ansi_256(1));
        assert_eq!(built.bg, Color::DEFAULT_BG);
        assert_eq!(built.attrs, StyleAttrs::empty());
    }

    #[test]
    fn test_build_style_with_bold() {
        let style = CurrentStyle::new(
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::BOLD,
            false,
        );
        let built = style.build_style();
        assert!(built.attrs.contains(StyleAttrs::BOLD));
    }

    #[test]
    fn test_build_extended_style_rgb_fg() {
        let style = CurrentStyle::new(
            PackedColor::rgb(100, 150, 200),
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
            false,
        );
        let ext = style.build_extended_style();
        assert_eq!(ext.fg_type, ColorType::Rgb);
        assert_eq!(ext.bg_type, ColorType::Default);
        assert_eq!(ext.style.fg, Color::new(100, 150, 200));
    }

    #[test]
    fn test_build_extended_style_indexed_bg() {
        let style = CurrentStyle::new(
            PackedColor::DEFAULT_FG,
            PackedColor::indexed(42),
            CellFlags::empty(),
            false,
        );
        let ext = style.build_extended_style();
        assert_eq!(ext.bg_type, ColorType::Indexed);
        assert_eq!(ext.bg_index, 42);
    }

    // =========================================================================
    // Partial-change build methods
    // =========================================================================

    #[test]
    fn test_build_extended_style_fg_changed() {
        let mut style = CurrentStyle::new(
            PackedColor::indexed(1),
            PackedColor::indexed(2),
            CellFlags::BOLD,
            false,
        );
        // Change only fg
        style.fg = PackedColor::rgb(10, 20, 30);
        let ext = style.build_extended_style_fg_changed();
        assert_eq!(ext.style.fg, Color::new(10, 20, 30));
        assert_eq!(ext.fg_type, ColorType::Rgb);
        // bg unchanged
        assert_eq!(ext.bg_type, ColorType::Indexed);
        assert_eq!(ext.bg_index, 2);
        // attrs unchanged
        assert!(ext.style.attrs.contains(StyleAttrs::BOLD));
    }

    #[test]
    fn test_build_extended_style_bg_changed() {
        let mut style = CurrentStyle::new(
            PackedColor::indexed(1),
            PackedColor::indexed(2),
            CellFlags::ITALIC,
            false,
        );
        style.bg = PackedColor::rgb(40, 50, 60);
        let ext = style.build_extended_style_bg_changed();
        assert_eq!(ext.style.bg, Color::new(40, 50, 60));
        assert_eq!(ext.bg_type, ColorType::Rgb);
        // fg unchanged
        assert_eq!(ext.fg_type, ColorType::Indexed);
        assert_eq!(ext.fg_index, 1);
    }

    #[test]
    fn test_build_extended_style_both_changed() {
        let mut style = CurrentStyle::new(
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
            false,
        );
        style.fg = PackedColor::indexed(7);
        style.bg = PackedColor::rgb(1, 2, 3);
        let ext = style.build_extended_style_both_changed();
        assert_eq!(ext.fg_type, ColorType::Indexed);
        assert_eq!(ext.fg_index, 7);
        assert_eq!(ext.bg_type, ColorType::Rgb);
        assert_eq!(ext.style.bg, Color::new(1, 2, 3));
    }

    // =========================================================================
    // Packed color accessors (fg_packed, bg_packed, flags_bits)
    // =========================================================================

    #[test]
    fn test_fg_packed_roundtrip() {
        let style = CurrentStyle::new(
            PackedColor::indexed(42),
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
            false,
        );
        assert_eq!(style.fg_packed(), PackedColor::indexed(42).0);
    }

    #[test]
    fn test_bg_packed_roundtrip() {
        let style = CurrentStyle::new(
            PackedColor::DEFAULT_FG,
            PackedColor::rgb(10, 20, 30),
            CellFlags::empty(),
            false,
        );
        assert_eq!(style.bg_packed(), PackedColor::rgb(10, 20, 30).0);
    }

    #[test]
    fn test_flags_bits_roundtrip() {
        let flags = CellFlags::BOLD.union(CellFlags::ITALIC);
        let style = CurrentStyle::new(
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            flags,
            false,
        );
        assert_eq!(style.flags_bits(), flags.bits());
    }

    // =========================================================================
    // SavedCursorState
    // =========================================================================

    #[test]
    fn test_saved_cursor_state_default() {
        let saved = SavedCursorState::default();
        assert_eq!(saved.cursor, Cursor::default());
        assert!(!saved.origin_mode);
        assert!(!saved.auto_wrap);
        assert!(!saved.pending_wrap);
        assert!(saved.underline_color.is_none());
    }

    #[test]
    fn test_saved_cursor_state_preserves_style() {
        let style = CurrentStyle::new(
            PackedColor::indexed(5),
            PackedColor::rgb(1, 2, 3),
            CellFlags::STRIKETHROUGH,
            true,
        );
        let saved = SavedCursorState {
            style,
            origin_mode: true,
            auto_wrap: true,
            pending_wrap: true,
            underline_color: Some(0x01_FF0000),
            ..SavedCursorState::default()
        };
        assert!(saved.style.protected);
        assert!(saved.style.flags.contains(CellFlags::STRIKETHROUGH));
        assert!(saved.origin_mode);
        assert!(saved.auto_wrap);
        assert!(saved.pending_wrap);
        assert_eq!(saved.underline_color, Some(0x01_FF0000));
    }

    // =========================================================================
    // Protected attribute behavior
    // =========================================================================

    #[test]
    fn test_protected_prevents_is_default() {
        let style = CurrentStyle::new(
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
            true,
        );
        assert!(
            !style.is_default(),
            "protected=true should prevent is_default()"
        );
    }

    #[test]
    fn test_new_with_all_fields() {
        let style = CurrentStyle::new(
            PackedColor::rgb(255, 0, 0),
            PackedColor::indexed(4),
            CellFlags::BOLD.union(CellFlags::UNDERLINE),
            true,
        );
        assert!(style.fg.is_rgb());
        assert!(style.bg.is_indexed());
        assert!(style.flags.contains(CellFlags::BOLD));
        assert!(style.flags.contains(CellFlags::UNDERLINE));
        assert!(style.protected);
        assert!(style.has_style_extras()); // RGB fg
        assert!(!style.is_default());
    }

    // =========================================================================
    // Extended flags trigger has_style_extras
    // =========================================================================

    #[test]
    fn test_superscript_triggers_style_extras() {
        let style = CurrentStyle::new(
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::SUPERSCRIPT,
            false,
        );
        assert!(
            style.has_style_extras(),
            "superscript is in EXTENDED_FLAGS_MASK"
        );
    }

    #[test]
    fn test_subscript_triggers_style_extras() {
        let style = CurrentStyle::new(
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::SUBSCRIPT,
            false,
        );
        assert!(style.has_style_extras());
    }

    #[test]
    fn test_basic_flags_no_style_extras() {
        let style = CurrentStyle::new(
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::BOLD
                .union(CellFlags::ITALIC)
                .union(CellFlags::UNDERLINE),
            false,
        );
        assert!(
            !style.has_style_extras(),
            "basic flags (bold+italic+underline) should not need style extras"
        );
    }
}
