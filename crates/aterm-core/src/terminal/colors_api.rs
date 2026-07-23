// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Hyperlink, underline color, CWD, color palette, and BiDi config accessors.
//!
//! Extracted from mod.rs to reduce file size.

use super::{ColorPalette, Rgb, Terminal};
use std::sync::Arc;

impl Terminal {
    /// Get the current hyperlink URL (OSC 8).
    ///
    /// Returns the URL that will be applied to newly printed characters.
    #[must_use]
    pub fn current_hyperlink(&self) -> Option<&Arc<str>> {
        self.transient.current_hyperlink.as_ref()
    }

    /// Set the current hyperlink URL (OSC 8).
    ///
    /// All subsequently printed characters will be linked to this URL.
    /// Pass `None` to clear the hyperlink.
    #[cfg(test)]
    pub fn set_current_hyperlink(&mut self, url: Option<Arc<str>>) {
        self.transient.current_hyperlink = url;
        self.transient.update_has_transient_extras();
    }

    /// Get the current hyperlink ID (OSC 8 `id=` parameter).
    ///
    /// Returns the ID used to group cells into the same hyperlink span.
    #[cfg(test)]
    #[must_use]
    pub fn current_hyperlink_id(&self) -> Option<&Arc<str>> {
        self.transient.current_hyperlink_id.as_ref()
    }

    /// Get the hyperlink URL attached to a rendered cell, if any.
    #[must_use]
    pub fn hyperlink_at(&self, row: u16, col: u16) -> Option<&str> {
        self.grid
            .cell_extra(row, col)
            .and_then(|extra| extra.hyperlink())
            .map(Arc::as_ref)
    }

    /// Get the hyperlink ID (OSC 8 `id=` parameter) attached to a rendered cell, if any.
    ///
    /// The `id=` parameter groups cells into the same hyperlink span. When present,
    /// two cells belong to the same hyperlink only if both the URL and `id=` match.
    #[must_use]
    pub fn hyperlink_id_at(&self, row: u16, col: u16) -> Option<&str> {
        self.grid
            .cell_extra(row, col)
            .and_then(|extra| extra.hyperlink_id())
            .map(Arc::as_ref)
    }

    /// Get the current underline color (SGR 58).
    ///
    /// Returns the underline color that will be applied to newly printed characters.
    /// Format: `0xTT_RRGGBB` where TT is 0x01 for RGB, 0x02 for indexed.
    #[cfg(test)]
    #[must_use]
    pub fn current_underline_color(&self) -> Option<u32> {
        self.transient.current_underline_color
    }

    /// Get the current working directory (OSC 7).
    ///
    /// Returns the path portion of the working directory URL set by the shell.
    /// The path is decoded from percent-encoding.
    #[must_use]
    pub fn current_working_directory(&self) -> Option<&str> {
        self.current_working_directory.as_deref()
    }

    /// Set the current working directory.
    ///
    /// This is typically set via OSC 7 from the shell.
    #[cfg(test)]
    pub fn set_current_working_directory(&mut self, path: Option<String>) {
        self.current_working_directory = path;
    }

    /// Get the color palette.
    ///
    /// The palette maps indexed colors (0-255) to RGB values. Use this to
    /// resolve indexed colors to their actual RGB values for rendering.
    #[must_use]
    pub fn color_palette(&self) -> &ColorPalette {
        &self.color.palette
    }

    /// Get a mutable reference to the color palette.
    pub fn color_palette_mut(&mut self) -> &mut ColorPalette {
        &mut self.color.palette
    }

    /// The RGB value for an indexed color.
    #[must_use]
    pub fn palette_color(&self, index: u8) -> Rgb {
        self.color.palette.get(index)
    }

    /// Indexed color as primitive RGB components.
    #[must_use]
    pub fn palette_color_components(&self, index: u8) -> (u8, u8, u8) {
        let color = self.palette_color(index);
        (color.r, color.g, color.b)
    }

    /// Set an indexed color in the palette.
    pub fn set_palette_color(&mut self, index: u8, color: Rgb) {
        self.color.palette.set(index, color);
    }

    /// Set indexed color from primitive RGB components.
    pub fn set_palette_color_components(&mut self, index: u8, r: u8, g: u8, b: u8) {
        self.set_palette_color(index, Rgb { r, g, b });
    }

    /// Reset the color palette to defaults.
    pub fn reset_color_palette(&mut self) {
        self.color.palette.reset();
    }

    /// Reset a single palette slot to the built-in default color.
    pub fn reset_palette_color_to_default(&mut self, index: u8) {
        let default_palette = ColorPalette::new();
        self.set_palette_color(index, default_palette.get(index));
    }

    /// Get the default foreground color.
    ///
    /// This is the color used for cells with default foreground styling.
    /// Modified via OSC 10, reset via OSC 110.
    #[must_use]
    pub fn default_foreground(&self) -> Rgb {
        self.color.default_foreground
    }

    /// Set the default foreground color.
    pub fn set_default_foreground(&mut self, color: Rgb) {
        self.color.default_foreground = color;
    }

    /// Get the default background color.
    ///
    /// This is the color used for cells with default background styling.
    /// Modified via OSC 11, reset via OSC 111.
    #[must_use]
    pub fn default_background(&self) -> Rgb {
        self.color.default_background
    }

    /// Set the default background color.
    pub fn set_default_background(&mut self, color: Rgb) {
        self.color.default_background = color;
    }

    /// Get the cursor color, if explicitly set.
    ///
    /// Returns `None` if the cursor uses the default foreground color.
    /// Modified via OSC 12, reset via OSC 112.
    #[must_use]
    pub fn cursor_color(&self) -> Option<Rgb> {
        self.color.cursor_color
    }

    /// Set the cursor color.
    ///
    /// Pass `None` to use the default foreground color.
    #[cfg(test)]
    pub fn set_cursor_color(&mut self, color: Option<Rgb>) {
        self.color.cursor_color = color;
    }

    /// Get the selection background color, if explicitly set.
    ///
    /// Returns `None` if the selection uses the renderer default color.
    /// Modified via OSC 21 selection_background.
    #[cfg(test)]
    #[must_use]
    pub fn selection_background(&self) -> Option<Rgb> {
        self.color.selection_background
    }
}
