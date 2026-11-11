// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal rendition state ([`CurrentStyle`], [`SavedCursorState`]).

use crate::cell::{CellFlags, PackedColor};
use crate::cell_colors::PackedColors;
use crate::cursor::Cursor;
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
    pub protected: bool,
    /// Cached packed colors, so a character write skips `convert_colors`.
    cached_colors: PackedColors,
    /// `flags.has_extended_flags() || fg.is_rgb() || bg.is_rgb()`.
    cached_has_style_extras: bool,
    /// The style is completely default, so `write_ascii_blast` may be used.
    cached_is_default: bool,
}

impl Default for CurrentStyle {
    #[inline]
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
    #[inline]
    pub const fn new(fg: PackedColor, bg: PackedColor, flags: CellFlags, protected: bool) -> Self {
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
    #[inline]
    pub const fn fg_packed(&self) -> u32 {
        self.fg.0
    }

    /// Background color as packed u32.
    #[must_use]
    #[inline]
    pub const fn bg_packed(&self) -> u32 {
        self.bg.0
    }

    /// Cell flags as raw u16 bits.
    #[must_use]
    #[inline]
    pub const fn flags_bits(&self) -> u16 {
        self.flags.bits()
    }

    /// Cached packed colors for writing cells without per-character conversion.
    #[must_use]
    #[inline]
    pub const fn cached_colors(&self) -> PackedColors {
        self.cached_colors
    }

    /// The background RGB for BCE when the background is truecolor.
    #[must_use]
    #[inline]
    pub const fn bce_bg_rgb(&self) -> Option<[u8; 3]> {
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
    pub const fn has_style_extras(&self) -> bool {
        self.cached_has_style_extras
    }

    /// Whether the style is completely default.
    #[must_use]
    #[inline]
    pub const fn is_default(&self) -> bool {
        self.cached_is_default
    }

    /// Recompute all cached state from current fg/bg/flags/protected.
    #[inline]
    pub const fn update_cached_colors(&mut self) {
        self.cached_colors = crate::Cell::convert_colors(self.fg, self.bg);
        self.cached_has_style_extras =
            self.flags.has_extended_flags() || self.fg.is_rgb() || self.bg.is_rgb();
        self.cached_is_default = self.fg.is_default()
            && self.bg.is_default()
            && self.flags.is_empty()
            && !self.protected;
    }

    /// Recompute only the flag-derived caches after a flags-only SGR change.
    #[inline]
    pub const fn update_flags_cache(&mut self) {
        self.cached_has_style_extras =
            self.flags.has_extended_flags() || self.fg.is_rgb() || self.bg.is_rgb();
        self.cached_is_default = self.fg.is_default()
            && self.bg.is_default()
            && self.flags.is_empty()
            && !self.protected;
    }

    /// Reset to default style (full reset including DECSCA protection).
    #[inline]
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Reset SGR attributes only (colors and flags).
    #[inline]
    pub fn reset_sgr(&mut self) {
        let protected = self.protected;
        *self = Self::default();
        self.protected = protected;
        self.cached_is_default = !protected;
    }
}

/// Saved cursor state for DECSC/DECRC.
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
    pub underline_color: Option<u32>,
    /// A resize changed the screen's size since the save, so the reflow may have moved a
    /// pending wrap off the line end it was armed at.
    pub resized: bool,
}

#[cfg(test)]
#[path = "terminal_style_tests.rs"]
mod tests;
