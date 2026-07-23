// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Style deduplication (Ghostty pattern).
//!
//! Most cells share styles. We store unique styles once and reference by ID.
//! This provides significant memory savings when many cells share the same
//! color/attribute combination.
//!
//! ## Memory Savings
//!
//! Without deduplication: Each cell stores colors + attributes inline (6 bytes).
//! With deduplication: Each cell stores a 2-byte style ID, styles are shared.
//!
//! For a typical terminal with 10K lines × 200 cols = 2M cells:
//! - Without: 2M × 6 = 12 MB for style data
//! - With: 2M × 2 + 100 styles × 12 = 4 MB + 1.2 KB ≈ 4 MB
//! - Savings: ~67% (or 3x better)
//!
//! Real-world terminals typically have 50-200 unique styles, not millions.

use std::fmt;

use super::cell::{CellFlags, PackedColor};

#[path = "style_color.rs"]
mod style_color;
pub use style_color::Color;

/// A style identifier.
///
/// Style IDs are indices into a `StyleTable`. The ID 0 is always the default
/// style (white on black, no attributes).
///
/// The inner `u16` is private; use [`new`](Self::new) / [`raw`](Self::raw)
/// for construction and access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct StyleId(u16);

impl StyleId {
    /// The default style ID (index 0).
    ///
    /// This is always valid and represents the default terminal style
    /// (white foreground, black background, no attributes).
    pub const DEFAULT: StyleId = StyleId(0);

    /// Create a `StyleId` from a raw `u16` index.
    #[must_use]
    #[inline]
    pub const fn new(raw: u16) -> Self {
        Self(raw)
    }

    /// Return the raw `u16` index.
    #[must_use]
    #[inline]
    pub const fn raw(self) -> u16 {
        self.0
    }

    /// Whether this is the default style (index 0).
    #[must_use]
    #[inline]
    pub const fn is_default(self) -> bool {
        self.0 == 0
    }
}

impl fmt::Display for StyleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "style:{}", self.0)
    }
}

impl From<StyleId> for u16 {
    #[inline]
    fn from(id: StyleId) -> Self {
        id.raw()
    }
}

impl From<u16> for StyleId {
    #[inline]
    fn from(value: u16) -> Self {
        Self::new(value)
    }
}

/// Text style combining colors and attributes.
///
/// A Style represents the visual appearance of a cell, including:
/// - Foreground color (text color)
/// - Background color
/// - Text attributes (bold, italic, underline, etc.)
///
/// Styles are immutable value types designed for efficient hashing and comparison.
/// The `StyleTable` interns styles so identical combinations share memory.
///
/// The default style is white text on black background with no attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct Style {
    /// Foreground color.
    pub fg: Color,
    /// Background color.
    pub bg: Color,
    /// Style attributes.
    pub attrs: StyleAttrs,
}

impl Default for Style {
    /// Default style: white text on black background, no attributes.
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Color type for style storage.
///
/// Used to track whether a color is default, indexed (palette), or RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
#[repr(u8)]
pub enum ColorType {
    /// Default terminal color (white fg, black bg).
    #[default]
    Default = 0,
    /// Indexed color (0-255 palette).
    Indexed = 1,
    /// True color RGB.
    Rgb = 2,
}

/// Extended style with color type information.
///
/// This struct stores the full style information including color types,
/// allowing conversion back to `PackedColors + CellFlags` format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ExtendedStyle {
    /// Core style (colors + attrs).
    pub style: Style,
    /// Foreground color type.
    pub fg_type: ColorType,
    /// Background color type.
    pub bg_type: ColorType,
    /// Foreground index (when fg_type == Indexed).
    pub fg_index: u8,
    /// Background index (when bg_type == Indexed).
    pub bg_index: u8,
}

impl Style {
    /// The default style (white text on black background, no attributes).
    pub const DEFAULT: Self = Self {
        fg: Color::DEFAULT_FG,
        bg: Color::DEFAULT_BG,
        attrs: StyleAttrs::empty(),
    };
}

impl ExtendedStyle {
    /// The default extended style.
    pub const DEFAULT: Self = Self {
        style: Style::DEFAULT,
        fg_type: ColorType::Default,
        bg_type: ColorType::Default,
        fg_index: 0,
        bg_index: 0,
    };

    /// Create from separate foreground and background PackedColor values.
    ///
    /// Used by Terminal's CurrentStyle which stores fg/bg as separate
    /// PackedColor values rather than the combined PackedColors format.
    #[must_use]
    pub fn from_packed_colors_separate(fg: PackedColor, bg: PackedColor, flags: CellFlags) -> Self {
        let (fg_color, fg_type, fg_index) = Self::unpack_color(fg, Color::DEFAULT_FG);
        let (bg_color, bg_type, bg_index) = Self::unpack_color(bg, Color::DEFAULT_BG);

        let attrs = Self::cell_flags_to_style_attrs(flags);

        Self {
            style: Style {
                fg: fg_color,
                bg: bg_color,
                attrs,
            },
            fg_type,
            bg_type,
            fg_index,
            bg_index,
        }
    }

    /// Unpack a `PackedColor` into its `(Color, ColorType, index)` components.
    fn unpack_color(packed: PackedColor, default: Color) -> (Color, ColorType, u8) {
        if packed.is_default() {
            (default, ColorType::Default, 0)
        } else if packed.is_indexed() {
            let idx = packed.index();
            (Color::from_ansi_256(idx), ColorType::Indexed, idx)
        } else if packed.is_rgb() {
            let (r, g, b) = packed.rgb_components();
            (Color::new(r, g, b), ColorType::Rgb, 0)
        } else {
            (default, ColorType::Default, 0)
        }
    }

    /// Bidirectional mapping between CellFlags and StyleAttrs.
    ///
    /// Adding a new style attribute requires only a single entry here;
    /// both conversion directions are derived from this table.
    const FLAG_ATTR_MAP: [(CellFlags, StyleAttrs); 15] = [
        (CellFlags::BOLD, StyleAttrs::BOLD),
        (CellFlags::DIM, StyleAttrs::DIM),
        (CellFlags::ITALIC, StyleAttrs::ITALIC),
        (CellFlags::UNDERLINE, StyleAttrs::UNDERLINE),
        (CellFlags::BLINK, StyleAttrs::BLINK),
        (CellFlags::INVERSE, StyleAttrs::INVERSE),
        (CellFlags::HIDDEN, StyleAttrs::HIDDEN),
        (CellFlags::STRIKETHROUGH, StyleAttrs::STRIKETHROUGH),
        (CellFlags::DOUBLE_UNDERLINE, StyleAttrs::DOUBLE_UNDERLINE),
        (CellFlags::CURLY_UNDERLINE, StyleAttrs::CURLY_UNDERLINE),
        (CellFlags::SUPERSCRIPT, StyleAttrs::SUPERSCRIPT),
        (CellFlags::SUBSCRIPT, StyleAttrs::SUBSCRIPT),
        // Compound styles: CellFlags uses bit combinations while StyleAttrs
        // has dedicated bits. Without these entries, cell_flags_to_style_attrs
        // only sets the individual component bits, causing dotted/dashed
        // underlines to be misidentified as curly in StyleAttrs consumers.
        (CellFlags::DOTTED_UNDERLINE, StyleAttrs::DOTTED_UNDERLINE),
        (CellFlags::DASHED_UNDERLINE, StyleAttrs::DASHED_UNDERLINE),
        (CellFlags::OVERLINE, StyleAttrs::OVERLINE),
    ];

    /// Convert CellFlags to StyleAttrs.
    #[must_use]
    pub fn cell_flags_to_style_attrs(flags: CellFlags) -> StyleAttrs {
        let mut attrs = StyleAttrs::empty();
        for &(cf, sa) in &Self::FLAG_ATTR_MAP {
            if flags.contains(cf) {
                attrs = attrs.union(sa);
            }
        }
        attrs
    }

    /// Convert StyleAttrs back to CellFlags.
    ///
    /// Only style-related flags are set; cell-specific flags like
    /// WIDE, WIDE_CONTINUATION, COMPLEX are not affected.
    #[must_use]
    pub fn attrs_to_cell_flags(attrs: StyleAttrs) -> CellFlags {
        let mut flags = CellFlags::empty();
        for &(cf, sa) in &Self::FLAG_ATTR_MAP {
            if attrs.contains(sa) {
                flags = flags.union(cf);
            }
        }
        flags
    }
}

aterm_types::bitflags! {
    /// Style attribute flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    #[repr(transparent)]
    pub struct StyleAttrs: u16 {
        /// Bold text.
        const BOLD = 1 << 0;
        /// Dim/faint text.
        const DIM = 1 << 1;
        /// Italic text.
        const ITALIC = 1 << 2;
        /// Underlined text.
        const UNDERLINE = 1 << 3;
        /// Blinking text.
        const BLINK = 1 << 4;
        /// Inverse video.
        const INVERSE = 1 << 5;
        /// Hidden/invisible text.
        const HIDDEN = 1 << 6;
        /// Strikethrough text.
        const STRIKETHROUGH = 1 << 7;
        /// Double underline.
        const DOUBLE_UNDERLINE = 1 << 8;
        /// Curly underline.
        const CURLY_UNDERLINE = 1 << 9;
        /// Dotted underline (SGR 4:4).
        const DOTTED_UNDERLINE = 1 << 10;
        /// Dashed underline (SGR 4:5).
        const DASHED_UNDERLINE = 1 << 11;
        /// Superscript text (SGR 73).
        const SUPERSCRIPT = 1 << 12;
        /// Subscript text (SGR 74).
        const SUBSCRIPT = 1 << 13;
        /// Overline text (SGR 53) — maps to SUPERSCRIPT | SUBSCRIPT in CellFlags.
        const OVERLINE = (1 << 12) | (1 << 13);
    }
}

// StyleTable struct and impl extracted to style_table.rs.
#[path = "style_table.rs"]
mod style_table;
#[cfg(any(test, kani, feature = "testing"))]
pub use style_table::ExtendedStyleInfo;
pub use style_table::StyleTable;

#[cfg(any(test, kani, feature = "testing"))]
#[path = "style_test_helpers.rs"]
mod style_test_helpers;
#[cfg(any(test, feature = "testing"))]
pub(crate) use style_test_helpers::StyleTableStats;
#[cfg(test)]
use style_test_helpers::take_style_intern_ops;

#[cfg(test)]
#[path = "style_tests.rs"]
mod tests;
