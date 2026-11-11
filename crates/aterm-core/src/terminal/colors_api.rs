// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Hyperlink, underline color, CWD, color palette, and BiDi config accessors.

use super::{ColorPalette, Rgb, Terminal};
use std::sync::Arc;

impl Terminal {
    /// The OSC 8 hyperlink URL applied to newly printed characters.
    #[must_use]
    #[inline]
    pub fn current_hyperlink(&self) -> Option<&Arc<str>> {
        self.transient
            .current_hyperlink
            .as_ref()
            .map(|link| &link.url)
    }

    /// Get the hyperlink URL attached to a rendered cell, if any.
    #[must_use]
    #[inline]
    pub fn hyperlink_at(&self, row: u16, col: u16) -> Option<&str> {
        self.grid
            .cell_extra(row, col)
            .and_then(|extra| extra.hyperlink())
            .map(Arc::as_ref)
    }

    /// Returns the hyperlink URL attached to the cell at viewport-relative `(visible_row, col)`.
    #[must_use]
    #[inline]
    pub fn hyperlink_at_visible(&self, visible_row: u16, col: u16) -> Option<Arc<str>> {
        let view = self.grid.visible_row_view(visible_row);
        let cell = view.cell(col)?;
        view.cell_data(col, cell)
            .cell_extra()
            .and_then(|extra| extra.hyperlink())
            .cloned()
    }

    /// Returns the hyperlink ID (OSC 8 `id=` parameter) attached to a rendered cell, if any.
    #[must_use]
    #[inline]
    pub fn hyperlink_id_at(&self, row: u16, col: u16) -> Option<&str> {
        self.grid
            .cell_extra(row, col)
            .and_then(|extra| extra.hyperlink_id())
            .map(Arc::as_ref)
    }

    /// Returns the percent-decoded working directory path reported by the shell (OSC 7).
    #[must_use]
    #[inline]
    pub fn current_working_directory(&self) -> Option<&str> {
        self.current_working_directory.as_deref()
    }

    /// Returns the non-local host named by the OSC 7 report that set [`Self::current_working_directory`].
    #[must_use]
    #[inline]
    pub fn reported_cwd_host(&self) -> Option<&str> {
        self.shell.cwd_host.as_deref()
    }

    /// Returns the sequence number of the CWD report that set [`Self::current_working_directory`], or `None` if carried from a checkpoint.
    #[must_use]
    #[inline]
    pub const fn reported_cwd_seq(&self) -> Option<u64> {
        if self.shell.cwd_carried {
            None
        } else {
            Some(self.shell.cwd_reports)
        }
    }

    /// The indexed (0-255) color palette.
    #[must_use]
    #[inline]
    pub const fn color_palette(&self) -> &ColorPalette {
        self.color.palette()
    }

    /// The RGB value for an indexed color.
    #[must_use]
    #[inline]
    pub const fn palette_color(&self, index: u8) -> Rgb {
        self.color.palette().get(index)
    }

    /// Sets a host-configured indexed color in the palette and its reset baseline.
    #[inline]
    pub fn set_palette_color(&mut self, index: u8, color: Rgb) {
        self.color.palette_mut().set(index, color);
        self.color
            .configured_palette
            .get_or_insert_with(ColorPalette::new)
            .set(index, color);
        self.grid.damage_mut().mark_full();
    }

    /// Set indexed color from primitive RGB components.
    #[inline]
    pub fn set_palette_color_components(&mut self, index: u8, r: u8, g: u8, b: u8) {
        self.set_palette_color(index, Rgb { r, g, b });
    }

    /// Returns the default foreground color.
    #[must_use]
    #[inline]
    pub const fn default_foreground(&self) -> Rgb {
        self.color.default_foreground
    }

    /// Sets the host-configured default foreground color and reset baseline.
    #[inline]
    pub fn set_default_foreground(&mut self, color: Rgb) {
        self.color.default_foreground = color;
        self.color.configured_foreground = color;
        self.color.frame_foreground_authoritative = true;
        self.grid.damage_mut().mark_full();
    }

    /// Returns the default background color.
    #[must_use]
    #[inline]
    pub const fn default_background(&self) -> Rgb {
        self.color.default_background
    }

    /// Sets the host-configured default background color and reset baseline.
    #[inline]
    pub fn set_default_background(&mut self, color: Rgb) {
        self.color.default_background = color;
        self.color.configured_background = color;
        self.color.frame_background_authoritative = true;
        self.grid.damage_mut().mark_full();
        let _ = self.reconcile_color_scheme();
    }

    /// Sets the host-configured cursor color and reset baseline.
    #[inline]
    pub fn set_default_cursor_color(&mut self, color: Option<Rgb>) {
        self.color.cursor_color = color;
        self.color.configured_cursor = color;
        self.color.frame_cursor_authoritative = true;
        self.grid.damage_mut().mark_full();
    }

    /// Returns the cursor color, if explicitly set.
    #[must_use]
    #[inline]
    pub const fn cursor_color(&self) -> Option<Rgb> {
        self.color.cursor_color
    }

    /// Returns whether the cursor color has been overridden via OSC 12 relative to the host baseline.
    #[must_use]
    #[inline]
    pub fn cursor_color_recoloured(&self) -> bool {
        self.color.cursor_color != self.color.configured_cursor
    }

    /// Returns the selection background color, if explicitly set.
    #[must_use]
    #[inline]
    pub const fn selection_background(&self) -> Option<Rgb> {
        self.color.selection_background
    }

    /// Sets the host-configured selection background color and reset baseline.
    #[inline]
    pub fn set_default_selection_background(&mut self, color: Option<Rgb>) {
        self.color.selection_background = color;
        self.color.configured_selection_background = color;
        self.grid.damage_mut().mark_full();
    }

    /// Returns the selection foreground color, if explicitly set.
    #[must_use]
    #[inline]
    pub const fn selection_foreground(&self) -> Option<Rgb> {
        self.color.selection_foreground
    }

    /// Sets the host-configured selection foreground color and reset baseline.
    #[inline]
    pub fn set_default_selection_foreground(&mut self, color: Option<Rgb>) {
        self.color.selection_foreground = color;
        self.color.configured_selection_foreground = color;
        self.grid.damage_mut().mark_full();
    }
}

#[cfg(test)]
#[path = "colors_api_tests.rs"]
mod tests;
