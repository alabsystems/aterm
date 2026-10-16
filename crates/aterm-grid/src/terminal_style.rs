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
#[cfg(test)]
use crate::style::{ExtendedStyle, Style};
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
    /// Whether characters are protected from selective erase (DECSCA): when
    /// true, DECSED/DECSEL skip them.
    pub protected: bool,
    /// Cached packed colors, so a character write skips `convert_colors`.
    cached_colors: PackedColors,
    /// `flags.has_extended_flags() || fg.is_rgb() || bg.is_rgb()`: the style
    /// alone requires CellExtras overflow.
    cached_has_style_extras: bool,
    /// The style is completely default, so `write_ascii_blast` may be used.
    cached_is_default: bool,
    // Only the caches the cell writers read are kept (`cached_colors` in
    // `write_char_core`, `has_style_extras` in the extras gate, `is_default` in
    // the ASCII-blast gate). Nothing in production interns a `Style` (no path
    // sets `CellFlags::USES_STYLE_ID`; cells store `PackedColors` inline), so
    // `build_style` / `build_extended_style*` compute on demand.
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
        }
    }
}

impl CurrentStyle {
    /// Construct from explicit fields (checkpoint deserialization).
    #[must_use]
    pub fn new(fg: PackedColor, bg: PackedColor, flags: CellFlags, protected: bool) -> Self {
        Self {
            fg,
            bg,
            flags,
            protected,
            cached_colors: crate::Cell::convert_colors(fg, bg),
            cached_has_style_extras: flags.has_extended_flags() || fg.is_rgb() || bg.is_rgb(),
            cached_is_default: fg.is_default() && bg.is_default() && flags.is_empty() && !protected,
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
    /// through erase/fill/scroll operations.
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
    }

    /// Recompute only the flag-derived caches after a flags-only SGR change.
    ///
    /// A flags-only SGR (e.g. `\x1b[1m`, `\x1b[7m`, `\x1b[22m`) leaves
    /// `fg`/`bg` alone, so `cached_colors` stays valid and only the two
    /// flag-dependent booleans are refreshed, avoiding `convert_colors` on every
    /// attribute toggle.
    #[inline]
    pub fn update_flags_cache(&mut self) {
        self.cached_has_style_extras =
            self.flags.has_extended_flags() || self.fg.is_rgb() || self.bg.is_rgb();
        self.cached_is_default = self.fg.is_default()
            && self.bg.is_default()
            && self.flags.is_empty()
            && !self.protected;
    }

    /// Build just the `Style` (fg, bg, attrs) that this rendition denotes.
    ///
    /// Computed on demand from `fg`/`bg`/`flags`. No production caller uses it;
    /// it is the round-trip/inspection API, so it must be correct rather than
    /// fast.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub fn build_style(&self) -> Style {
        self.build_extended_style().style
    }

    /// Build the `ExtendedStyle` (colors + color types + palette indices) that
    /// this rendition denotes, via one `from_packed_colors_separate` call.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub fn build_extended_style(&self) -> ExtendedStyle {
        ExtendedStyle::from_packed_colors_separate(self.fg, self.bg, self.flags)
    }

    /// Refresh the writer caches after only the fg color changed, and return
    /// the resulting `ExtendedStyle`.
    ///
    /// The fg/bg/both variants all recompute the caches wholesale.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub fn build_extended_style_fg_changed(&mut self) -> ExtendedStyle {
        self.update_cached_colors();
        self.build_extended_style()
    }

    /// Refresh the writer caches after only the bg color changed, and return
    /// the resulting `ExtendedStyle`.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub fn build_extended_style_bg_changed(&mut self) -> ExtendedStyle {
        self.update_cached_colors();
        self.build_extended_style()
    }

    /// Refresh the writer caches after both fg and bg changed, and return the
    /// resulting `ExtendedStyle`.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub fn build_extended_style_both_changed(&mut self) -> ExtendedStyle {
        self.update_cached_colors();
        self.build_extended_style()
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
    /// Pending wrap (wrap-next) flag at save time.
    pub pending_wrap: bool,
    /// Underline color at save time (SGR 58/59).
    ///
    /// Stored as packed u32 color (same encoding as `PackedColor`).
    /// `None` means default underline color (foreground).
    pub underline_color: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    // StyleAttrs is used only by these tests.
    use crate::style::StyleAttrs;

    // =========================================================================
    // CurrentStyle::default, and one attribute or colour changed from it
    // =========================================================================

    #[test]
    fn default_style_is_empty_and_cached_as_default() {
        let style = CurrentStyle::default();
        assert!(style.fg.is_default(), "fg is DEFAULT_FG");
        assert!(style.bg.is_default(), "bg is DEFAULT_BG");
        assert!(style.flags.is_empty(), "no flags");
        assert!(!style.protected, "not protected");
        assert!(style.is_default(), "cached is_default");
        assert!(!style.has_style_extras(), "no style extras");
        assert_eq!(
            style.build_style().attrs,
            StyleAttrs::empty(),
            "the default rendition denotes empty StyleAttrs"
        );
    }

    #[test]
    fn one_attribute_or_colour_leaves_the_default() {
        // (label, flag, fg, bg, needs style extras).
        // Style extras are needed for the extended flag bits (11-13) and RGB.
        let d_fg = PackedColor::DEFAULT_FG;
        let d_bg = PackedColor::DEFAULT_BG;
        let empty = CellFlags::empty();
        let rows = [
            ("bold", CellFlags::BOLD, d_fg, d_bg, false),
            ("italic", CellFlags::ITALIC, d_fg, d_bg, false),
            ("underline", CellFlags::UNDERLINE, d_fg, d_bg, false),
            ("dim", CellFlags::DIM, d_fg, d_bg, false),
            ("blink", CellFlags::BLINK, d_fg, d_bg, false),
            ("inverse", CellFlags::INVERSE, d_fg, d_bg, false),
            ("hidden", CellFlags::HIDDEN, d_fg, d_bg, false),
            ("strikethrough", CellFlags::STRIKETHROUGH, d_fg, d_bg, false),
            (
                "overline (extended bits)",
                CellFlags::OVERLINE,
                d_fg,
                d_bg,
                true,
            ),
            ("indexed fg 1", empty, PackedColor::indexed(1), d_bg, false),
            ("indexed bg 4", empty, d_fg, PackedColor::indexed(4), false),
            ("rgb fg", empty, PackedColor::rgb(255, 128, 0), d_bg, true),
            ("rgb bg", empty, d_fg, PackedColor::rgb(0, 64, 128), true),
        ];
        for (label, flag, fg, bg, extras) in rows {
            let mut style = CurrentStyle {
                fg,
                bg,
                ..CurrentStyle::default()
            };
            style.flags.insert(flag);
            style.update_cached_colors();
            assert!(style.flags.contains(flag), "{label}: flag set");
            assert_eq!(style.fg, fg, "{label}: fg");
            assert_eq!(style.bg, bg, "{label}: bg");
            assert!(!style.is_default(), "{label}: no longer default");
            assert_eq!(style.has_style_extras(), extras, "{label}: style extras");
        }
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
    // Several colours at once
    // =========================================================================

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

    // =========================================================================
    // Style comparison / equality
    // =========================================================================

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

    // =========================================================================
    // build_style / build_extended_style correctness
    // =========================================================================

    // =========================================================================
    // Partial-change build methods
    // =========================================================================

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
