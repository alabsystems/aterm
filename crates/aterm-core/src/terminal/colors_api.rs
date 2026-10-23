// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Hyperlink, underline color, CWD, color palette, and BiDi config accessors.

use super::{ColorPalette, Rgb, Terminal};
use std::sync::Arc;

impl Terminal {
    /// The OSC 8 hyperlink URL applied to newly printed characters.
    #[must_use]
    #[inline]
    pub const fn current_hyperlink(&self) -> Option<&Arc<str>> {
        self.transient.current_hyperlink.as_ref()
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
mod tests {
    use super::*;

    #[test]
    fn configured_colors_survive_dynamic_color_set_and_reset() {
        let foreground = Rgb::new(0x11, 0x22, 0x33);
        let background = Rgb::new(0x44, 0x55, 0x66);
        let mut terminal = Terminal::new(24, 80);
        terminal.set_default_foreground(foreground);
        terminal.set_default_background(background);

        terminal.process(b"\x1b]10;rgb:aa/bb/cc\x07\x1b]11;rgb:77/88/99\x07");
        assert_ne!(terminal.default_foreground(), foreground);
        assert_ne!(terminal.default_background(), background);

        terminal.process(b"\x1b]110\x07\x1b]111\x07");
        assert_eq!(
            terminal.default_foreground(),
            foreground,
            "OSC 110 restores the configured foreground"
        );
        assert_eq!(
            terminal.default_background(),
            background,
            "OSC 111 restores the configured background"
        );
    }

    fn assert_host_color_mutation_marks_damage(
        setup: impl FnOnce(&mut Terminal),
        mutate: impl FnOnce(&mut Terminal),
    ) {
        let mut term = Terminal::new(2, 8);
        setup(&mut term);
        term.take_damage();
        let before = term.damage_epoch();
        assert!(!term.has_damage());

        mutate(&mut term);

        assert!(
            term.has_damage(),
            "host color mutation must repaint even with no glyph write"
        );
        assert!(
            term.damage_epoch() > before,
            "host color mutation must advance the frontend redraw key"
        );
    }

    #[test]
    fn public_color_mutators_mark_full_damage_without_a_glyph_write() {
        let noop = |_: &mut Terminal| {};
        assert_host_color_mutation_marks_damage(noop, |term| {
            term.set_palette_color(1, Rgb::new(0x11, 0x22, 0x33));
        });
        assert_host_color_mutation_marks_damage(noop, |term| {
            term.set_default_foreground(Rgb::new(0x11, 0x22, 0x33));
        });
        assert_host_color_mutation_marks_damage(noop, |term| {
            term.set_default_background(Rgb::new(0x44, 0x55, 0x66));
        });
        assert_host_color_mutation_marks_damage(noop, |term| {
            term.set_default_cursor_color(Some(Rgb::new(0x77, 0x88, 0x99)));
        });
        assert_host_color_mutation_marks_damage(noop, |term| {
            term.set_default_selection_background(Some(Rgb::new(0x11, 0x44, 0x77)));
        });
        assert_host_color_mutation_marks_damage(noop, |term| {
            term.set_default_selection_foreground(Some(Rgb::new(0x99, 0x66, 0x33)));
        });
    }

    #[test]
    fn public_default_setters_seed_osc_reset_baselines() {
        let mut term = Terminal::new(2, 8);
        let foreground = Rgb::new(0x11, 0x22, 0x33);
        let background = Rgb::new(0x44, 0x55, 0x66);
        let cursor = Rgb::new(0x77, 0x88, 0x99);
        let selection_bg = Rgb::new(0x12, 0x45, 0x78);
        let selection_fg = Rgb::new(0x98, 0x76, 0x54);
        term.set_default_foreground(foreground);
        term.set_default_background(background);
        term.set_default_cursor_color(Some(cursor));
        term.set_default_selection_background(Some(selection_bg));
        term.set_default_selection_foreground(Some(selection_fg));

        term.process(
            b"\x1b]10;rgb:aa/bb/cc\x07\x1b]11;rgb:77/88/99\x07\
              \x1b]12;rgb:dd/ee/ff\x07\x1b]17;rgb:01/02/03\x07\
              \x1b]19;rgb:04/05/06\x07",
        );
        assert_ne!(term.default_foreground(), foreground);
        assert_ne!(term.default_background(), background);
        assert_ne!(term.cursor_color(), Some(cursor));
        assert_ne!(term.selection_background(), Some(selection_bg));
        assert_ne!(term.selection_foreground(), Some(selection_fg));

        term.process(b"\x1b]110\x07\x1b]111\x07\x1b]112\x07\x1b]117\x07\x1b]119\x07");
        assert_eq!(term.default_foreground(), foreground);
        assert_eq!(term.default_background(), background);
        assert_eq!(
            term.cursor_color(),
            Some(cursor),
            "OSC 112 restores the configured host cursor"
        );
        assert_eq!(term.selection_background(), Some(selection_bg));
        assert_eq!(term.selection_foreground(), Some(selection_fg));
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
}
