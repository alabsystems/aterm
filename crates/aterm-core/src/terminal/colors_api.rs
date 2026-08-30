// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Hyperlink, underline color, CWD, color palette, and BiDi config accessors.
//!
//! Extracted from mod.rs to reduce file size.

use super::{ColorPalette, Rgb, Terminal};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

/// Mutable host-configuration view of the indexed palette.
///
/// Direct palette edits are mirrored into the OSC 104/RIS reset baseline on
/// drop. The before/after comparison updates only slots the caller actually
/// changed, so an unrelated program-issued OSC 4 override can never leak into
/// the host baseline.
pub struct ColorPaletteMut<'a> {
    palette: &'a mut ColorPalette,
    configured_palette: &'a mut Option<ColorPalette>,
    before: ColorPalette,
}

impl Deref for ColorPaletteMut<'_> {
    type Target = ColorPalette;

    fn deref(&self) -> &Self::Target {
        self.palette
    }
}

impl DerefMut for ColorPaletteMut<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.palette
    }
}

impl Drop for ColorPaletteMut<'_> {
    fn drop(&mut self) {
        let configured = self
            .configured_palette
            .get_or_insert_with(ColorPalette::new);
        for index in u8::MIN..=u8::MAX {
            let color = self.palette.get(index);
            if color != self.before.get(index) {
                configured.set(index, color);
            }
        }
    }
}

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

    /// The hyperlink URL attached to the cell at VIEWPORT row `visible_row` —
    /// the display-offset-AWARE twin of [`Self::hyperlink_at`], and the one a
    /// host must ask when it is answering for what a person is LOOKING AT.
    ///
    /// [`Self::hyperlink_at`] keys the extras map by the LIVE screen row, so
    /// scrolled back it answers about a line eight rows further down (or about
    /// a blank one) while the frame under the pointer was drawn from history.
    /// This resolves through the same [`Grid::visible_row_view`] the frame
    /// itself is drawn from ([`Self::render_row`]), so the answer and the
    /// underline the renderer stamped always describe one cell. The two
    /// coincide at `display_offset == 0`.
    ///
    /// Owned rather than borrowed because a scrolled-off row is MATERIALIZED:
    /// its extras belong to the view, so no reference can outlive the
    /// resolution.
    ///
    /// [`Grid::visible_row_view`]: aterm_grid::Grid::visible_row_view
    #[must_use]
    pub fn hyperlink_at_visible(&self, visible_row: u16, col: u16) -> Option<Arc<str>> {
        let view = self.grid.visible_row_view(visible_row);
        let cell = view.cell(col)?;
        view.cell_data(col, cell)
            .cell_extra()
            .and_then(|extra| extra.hyperlink())
            .cloned()
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

    /// Mutate the host-configured color palette.
    ///
    /// Changed slots become both the live colors and their OSC 104/RIS reset
    /// baseline, matching [`Self::set_palette_color`].
    pub fn color_palette_mut(&mut self) -> ColorPaletteMut<'_> {
        // A mutable palette borrow can recolor already-painted indexed cells
        // without touching grid content. Mark before handing the borrow out so
        // every damage-keyed frontend observes the possible visual mutation.
        self.grid.damage_mut().mark_full();
        let before = self.color.palette.clone();
        ColorPaletteMut {
            palette: &mut self.color.palette,
            configured_palette: &mut self.color.configured_palette,
            before,
        }
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
        self.grid.damage_mut().mark_full();
    }

    /// Set indexed color from primitive RGB components.
    pub fn set_palette_color_components(&mut self, index: u8, r: u8, g: u8, b: u8) {
        self.set_palette_color(index, Rgb { r, g, b });
    }

    /// Reset the live and host-configured color palette to built-in defaults.
    pub fn reset_color_palette(&mut self) {
        self.color.palette.reset();
        self.color.configured_palette = None;
        self.grid.damage_mut().mark_full();
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
        self.color.frame_foreground_authoritative = true;
        self.grid.damage_mut().mark_full();
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
        self.color.frame_background_authoritative = true;
        self.grid.damage_mut().mark_full();
    }

    /// Set the host-configured cursor color.
    ///
    /// This seeds both the live OSC 12 value and the OSC 112/RIS reset
    /// baseline. `None` makes the configured fallback the default foreground.
    pub fn set_default_cursor_color(&mut self, color: Option<Rgb>) {
        self.color.cursor_color = color;
        self.color.configured_cursor = color;
        self.color.frame_cursor_authoritative = true;
        self.grid.damage_mut().mark_full();
    }

    /// Get the cursor color, if explicitly set.
    ///
    /// Returns `None` if the cursor uses the default foreground color.
    /// Modified via OSC 12, reset via OSC 112.
    #[must_use]
    pub fn cursor_color(&self) -> Option<Rgb> {
        self.color.cursor_color
    }

    /// Whether a program has RECOLOURED the cursor over the host's baseline —
    /// a live OSC 12 that OSC 112 / RIS has not yet reset.
    ///
    /// [`Self::set_default_cursor_color`] seeds BOTH the live value and the
    /// reset baseline from the theme, so [`Self::cursor_color`] being `Some`
    /// says nothing about whether anyone asked for that colour: on every
    /// default window it is the theme's own seed. The two slots diverge only
    /// when OSC 12 writes the live one, and re-converge when OSC 112 restores
    /// the baseline. That divergence is the one honest answer to "did a
    /// program pin this colour?", and it is what a cursor effect needs to
    /// decide whether the caret wears the terminal's colour or its own.
    #[must_use]
    pub fn cursor_color_recoloured(&self) -> bool {
        self.color.cursor_color != self.color.configured_cursor
    }

    /// Get the selection background color, if explicitly set.
    ///
    /// Returns `None` if the selection uses the renderer default color.
    /// Modified via OSC 17 / OSC 21 `selection_background`.
    #[must_use]
    pub fn selection_background(&self) -> Option<Rgb> {
        self.color.selection_background
    }

    /// Set the host-configured selection background.
    ///
    /// Seeds both the live OSC 17/21 value and the OSC 117/RIS baseline.
    /// `None` delegates selection fill to the renderer theme.
    pub fn set_default_selection_background(&mut self, color: Option<Rgb>) {
        self.color.selection_background = color;
        self.color.configured_selection_background = color;
        self.grid.damage_mut().mark_full();
    }

    /// Get the selection foreground color, if explicitly set.
    ///
    /// Returns `None` if selected text uses the renderer's automatic/default
    /// foreground. Modified via OSC 19 and reset via OSC 119.
    #[must_use]
    pub fn selection_foreground(&self) -> Option<Rgb> {
        self.color.selection_foreground
    }

    /// Set the host-configured selection foreground.
    ///
    /// Seeds both the live OSC 19 value and the OSC 119/RIS baseline. `None`
    /// leaves selected-text contrast resolution to the renderer.
    pub fn set_default_selection_foreground(&mut self, color: Option<Rgb>) {
        self.color.selection_foreground = color;
        self.color.configured_selection_foreground = color;
        self.grid.damage_mut().mark_full();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_host_color_mutation_marks_damage(
            |term| term.set_palette_color(1, Rgb::new(0x11, 0x22, 0x33)),
            Terminal::reset_color_palette,
        );
        assert_host_color_mutation_marks_damage(
            |term| term.set_palette_color(1, Rgb::new(0x11, 0x22, 0x33)),
            |term| term.reset_palette_color_to_default(1),
        );
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
        assert_host_color_mutation_marks_damage(noop, |term| {
            term.color_palette_mut().set(2, Rgb::new(0x77, 0x88, 0x99));
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

    #[test]
    fn mutable_palette_view_updates_only_changed_reset_slots() {
        let mut term = Terminal::new(2, 8);
        term.set_allow_palette_reconfigure(true);
        let configured = Rgb::new(0x11, 0x22, 0x33);
        let transient = Rgb::new(0xaa, 0xbb, 0xcc);
        let built_in_two = ColorPalette::new().get(2);

        term.process(b"\x1b]4;2;rgb:aa/bb/cc\x07");
        {
            let mut palette = term.color_palette_mut();
            palette.set(1, configured);
        }
        assert_eq!(term.palette_color(2), transient);

        term.process(b"\x1b]4;1;rgb:aa/bb/cc\x07\x1b]104\x07");
        assert_eq!(
            term.palette_color(1),
            configured,
            "changed slot becomes a durable host reset baseline"
        );
        assert_eq!(
            term.palette_color(2),
            built_in_two,
            "an unrelated OSC 4 override is not captured into that baseline"
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
