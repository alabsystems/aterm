// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Test-only helpers for grid style internals.

use super::*;
use crate::PackedColors;

// Counter for style intern operations (O(1) verification).
// NOTE: Use full path std::cell::Cell to avoid conflict with grid Cell type.
thread_local! {
    static STYLE_INTERN_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Increment the style intern operation counter.
fn count_style_intern_op() {
    STYLE_INTERN_OPS.with(|c| c.set(c.get() + 1));
}

/// Take (read and reset) the style intern operation count.
#[cfg(test)]
pub fn take_style_intern_ops() -> usize {
    STYLE_INTERN_OPS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

impl Color {
    #[must_use]
    #[inline]
    pub const fn with_alpha(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    #[must_use]
    #[inline]
    pub const fn from_rgb(rgb: (u8, u8, u8)) -> Self {
        Self::new(rgb.0, rgb.1, rgb.2)
    }

    #[must_use]
    #[inline]
    pub const fn is_default_fg(self) -> bool {
        self.r == 255 && self.g == 255 && self.b == 255 && self.a == 255
    }

    #[must_use]
    #[inline]
    pub const fn is_default_bg(self) -> bool {
        self.r == 0 && self.g == 0 && self.b == 0 && self.a == 255
    }
}

impl Style {
    /// Create a new style with the given colors and attributes.
    #[must_use]
    #[inline]
    pub const fn new(fg: Color, bg: Color, attrs: StyleAttrs) -> Self {
        Self { fg, bg, attrs }
    }

    /// Create a style with just foreground color.
    #[must_use]
    #[inline]
    pub const fn with_fg(fg: Color) -> Self {
        Self {
            fg,
            bg: Color::DEFAULT_BG,
            attrs: StyleAttrs::empty(),
        }
    }

    /// Create a style with just background color.
    #[must_use]
    #[inline]
    pub const fn with_bg(bg: Color) -> Self {
        Self {
            fg: Color::DEFAULT_FG,
            bg,
            attrs: StyleAttrs::empty(),
        }
    }

    /// Create a style with just attributes.
    #[must_use]
    #[inline]
    pub const fn with_attrs(attrs: StyleAttrs) -> Self {
        Self {
            fg: Color::DEFAULT_FG,
            bg: Color::DEFAULT_BG,
            attrs,
        }
    }

    /// Check if this is the default style.
    #[must_use]
    #[inline]
    pub const fn is_default(&self) -> bool {
        self.fg.is_default_fg() && self.bg.is_default_bg() && self.attrs.is_empty()
    }

    /// Return a style with the foreground color changed.
    #[must_use]
    #[inline]
    pub const fn set_fg(self, fg: Color) -> Self {
        Self { fg, ..self }
    }

    /// Return a style with the background color changed.
    #[must_use]
    #[inline]
    pub const fn set_bg(self, bg: Color) -> Self {
        Self { bg, ..self }
    }

    /// Return a style with the attributes changed.
    #[must_use]
    #[inline]
    pub const fn set_attrs(self, attrs: StyleAttrs) -> Self {
        Self { attrs, ..self }
    }
}

impl ExtendedStyle {
    /// Create from PackedColors and CellFlags (test/conversion utility).
    #[must_use]
    pub fn from_cell_style(
        colors: PackedColors,
        flags: CellFlags,
        fg_rgb: Option<(u8, u8, u8)>,
        bg_rgb: Option<(u8, u8, u8)>,
    ) -> Self {
        let (fg, fg_type, fg_index) = if colors.fg_is_default() {
            (Color::DEFAULT_FG, ColorType::Default, 0)
        } else if colors.fg_is_indexed() {
            let idx = colors.fg_index();
            (Color::DEFAULT_FG, ColorType::Indexed, idx)
        } else if colors.fg_is_rgb() {
            let rgb = fg_rgb.unwrap_or((255, 255, 255));
            (Color::from_rgb(rgb), ColorType::Rgb, 0)
        } else {
            (Color::DEFAULT_FG, ColorType::Default, 0)
        };

        let (bg, bg_type, bg_index) = if colors.bg_is_default() {
            (Color::DEFAULT_BG, ColorType::Default, 0)
        } else if colors.bg_is_indexed() {
            let idx = colors.bg_index();
            (Color::DEFAULT_BG, ColorType::Indexed, idx)
        } else if colors.bg_is_rgb() {
            let rgb = bg_rgb.unwrap_or((0, 0, 0));
            (Color::from_rgb(rgb), ColorType::Rgb, 0)
        } else {
            (Color::DEFAULT_BG, ColorType::Default, 0)
        };

        let attrs = Self::cell_flags_to_style_attrs(flags);

        Self {
            style: Style { fg, bg, attrs },
            fg_type,
            bg_type,
            fg_index,
            bg_index,
        }
    }

    /// Convert back to PackedColors.
    ///
    /// Note: For RGB colors, the actual RGB values are stored separately.
    /// This method only sets the color mode indicators.
    #[must_use]
    pub fn to_packed_colors(self) -> PackedColors {
        let mut colors = PackedColors::DEFAULT;

        match self.fg_type {
            ColorType::Default => {}
            ColorType::Indexed => {
                colors = colors.set_fg_indexed(self.fg_index);
            }
            ColorType::Rgb => {
                colors = colors.with_rgb_fg();
            }
        }

        match self.bg_type {
            ColorType::Default => {}
            ColorType::Indexed => {
                colors = colors.set_bg_indexed(self.bg_index);
            }
            ColorType::Rgb => {
                colors = colors.with_rgb_bg();
            }
        }

        colors
    }
}

impl StyleTable {
    /// Intern a style, returning its ID.
    ///
    /// If the style already exists, increments its reference count and returns
    /// the existing ID. Otherwise, creates a new style entry.
    ///
    /// # Performance
    ///
    /// O(1) average case (hash lookup).
    pub fn intern(&mut self, style: Style) -> StyleId {
        count_style_intern_op();

        if let Some(&id) = self.lookup.get(&style) {
            self.ref_counts[id.raw() as usize] =
                self.ref_counts[id.raw() as usize].saturating_add(1);
            return id;
        }

        self.insert_new_style(style, None)
    }

    /// Intern a style without incrementing reference count.
    #[must_use]
    pub fn get_id(&self, style: &Style) -> Option<StyleId> {
        self.lookup.get(style).copied()
    }

    /// Add a reference to an existing style.
    #[inline]
    pub fn add_ref(&mut self, id: StyleId) {
        let idx = id.raw() as usize;
        if idx < self.ref_counts.len() {
            self.ref_counts[idx] = self.ref_counts[idx].saturating_add(1);
        }
    }

    /// Get the number of unique styles.
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.styles.len()
    }

    /// Returns true if the table has only the default style.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.styles.len() <= 1
    }

    /// Get the reference count for a style.
    #[must_use]
    pub fn ref_count(&self, id: StyleId) -> u32 {
        self.ref_counts.get(id.raw() as usize).copied().unwrap_or(0)
    }

    /// Get the number of styles with non-zero reference counts.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.ref_counts.iter().filter(|&&c| c > 0).count()
    }

    /// Estimate memory usage in bytes.
    #[must_use]
    pub fn memory_used(&self) -> usize {
        let styles_size = self.styles.capacity() * std::mem::size_of::<Style>();
        let ref_counts_size = self.ref_counts.capacity() * std::mem::size_of::<u32>();
        let extended_size =
            self.extended.capacity() * std::mem::size_of::<Option<ExtendedStyleInfo>>();
        let lookup_size = self.lookup.capacity() * (std::mem::size_of::<Style>() + 8);

        styles_size + ref_counts_size + extended_size + lookup_size
    }

    /// Get statistics about the style table.
    #[must_use]
    pub fn stats(&self) -> StyleTableStats {
        let total = self.styles.len();
        let active = self.active_count();
        let total_refs: u64 = self.ref_counts.iter().map(|&c| u64::from(c)).sum();

        StyleTableStats {
            total_styles: total,
            active_styles: active,
            total_refs,
            memory_bytes: self.memory_used(),
        }
    }
}

/// Statistics about a StyleTable.
#[derive(Debug, Clone, Copy)]
pub struct StyleTableStats {
    /// Total number of unique styles.
    pub total_styles: usize,
    /// Number of styles with non-zero reference counts.
    pub active_styles: usize,
    /// Total reference count across all styles.
    pub total_refs: u64,
    /// Estimated memory usage in bytes.
    pub memory_bytes: usize,
}
