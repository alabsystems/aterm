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

    /// Set a host-configured indexed color in the palette.
    ///
    /// This seeds both the live slot and its reset baseline: a later OSC 4 may
    /// replace the live value, while OSC 104 and RIS restore `color`. The first
    /// configured slot starts from the built-in palette rather than cloning the
    /// live palette, so a prior program-issued OSC 4 mutation can never become a
    /// host reset default accidentally.
    pub fn set_palette_color(&mut self, index: u8, color: Rgb) {
        self.color.palette.set(index, color);
        self.color
            .configured_palette
            .get_or_insert_with(ColorPalette::new)
            .set(index, color);
    }

    /// Set indexed color from primitive RGB components.
    pub fn set_palette_color_components(&mut self, index: u8, r: u8, g: u8, b: u8) {
        self.set_palette_color(index, Rgb { r, g, b });
    }

    /// Reset the live and host-configured color palette to built-in defaults.
    pub fn reset_color_palette(&mut self) {
        self.color.palette.reset();
        self.color.configured_palette = None;
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

    /// Set the host-configured default foreground color.
    ///
    /// This seeds both the live value and the reset baseline: a later OSC 10
    /// may replace the live value, while OSC 110 restores `color`.
    pub fn set_default_foreground(&mut self, color: Rgb) {
        self.color.default_foreground = color;
        self.color.configured_foreground = color;
    }

    /// Get the default background color.
    ///
    /// This is the color used for cells with default background styling.
    /// Modified via OSC 11, reset via OSC 111.
    #[must_use]
    pub fn default_background(&self) -> Rgb {
        self.color.default_background
    }

    /// Set the host-configured default background color.
    ///
    /// This seeds both the live value and the reset baseline: a later OSC 11
    /// may replace the live value, while OSC 111 restores `color`.
    pub fn set_default_background(&mut self, color: Rgb) {
        self.color.default_background = color;
        self.color.configured_background = color;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_default_setters_seed_osc_reset_baselines() {
        let mut term = Terminal::new(2, 8);
        let foreground = Rgb::new(0x11, 0x22, 0x33);
        let background = Rgb::new(0x44, 0x55, 0x66);
        term.set_default_foreground(foreground);
        term.set_default_background(background);

        term.process(b"\x1b]10;rgb:aa/bb/cc\x07\x1b]11;rgb:77/88/99\x07");
        assert_ne!(term.default_foreground(), foreground);
        assert_ne!(term.default_background(), background);

        term.process(b"\x1b]110\x07\x1b]111\x07");
        assert_eq!(term.default_foreground(), foreground);
        assert_eq!(term.default_background(), background);
    }

    #[test]
    fn public_palette_setter_seeds_osc_and_ris_reset_baseline() {
        let mut term = Terminal::new(2, 8);
        term.set_allow_palette_reconfigure(true);
        let configured = Rgb::new(0x11, 0x22, 0x33);
        let transient = Rgb::new(0xaa, 0xbb, 0xcc);
        term.set_palette_color(1, configured);

        term.process(b"\x1b]4;1;rgb:aa/bb/cc\x07");
        assert_eq!(term.palette_color(1), transient);
        term.process(b"\x1b]104;1\x07");
        assert_eq!(
            term.palette_color(1),
            configured,
            "OSC 104 restores the host-configured palette slot"
        );

        term.process(b"\x1b]4;1;rgb:aa/bb/cc\x07");
        term.process(b"\x1bc");
        assert_eq!(
            term.palette_color(1),
            configured,
            "RIS restores the same host-configured palette baseline"
        );
    }

    #[test]
    fn public_palette_reset_durably_restores_built_in_baseline() {
        let mut term = Terminal::new(2, 8);
        term.set_allow_palette_reconfigure(true);
        let built_in = ColorPalette::new().get(1);
        term.set_palette_color_components(1, 0x11, 0x22, 0x33);
        term.reset_color_palette();
        assert_eq!(term.palette_color(1), built_in);

        term.process(b"\x1b]4;1;rgb:aa/bb/cc\x07");
        term.process(b"\x1b]104;1\x07");
        assert_eq!(
            term.palette_color(1),
            built_in,
            "OSC 104 must not resurrect the cleared host palette"
        );
    }
}
