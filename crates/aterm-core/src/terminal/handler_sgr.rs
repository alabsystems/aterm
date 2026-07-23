// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! SGR (Select Graphic Rendition) handler for the terminal.
//!
//! This module contains handlers for text styling escape sequences:
//! - Basic text attributes (bold, italic, underline, etc.)
//! - Foreground and background colors (8/16/256/true color)
//! - Underline colors and styles
//! - Superscript and subscript
//! - Colon-separated subparameter parsing (ISO 8613-3)
//!
//! Extracted from handler.rs as part of #485 (large files refactor).

use crate::grid::{CellFlags, PackedColor, StyleId};

use super::handler::SgrStyleHandler;
use super::sgr_color_u8;

impl SgrStyleHandler<'_> {
    /// Update the cached style ID from the current style state.
    ///
    /// This interns the current style (fg, bg, flags) into the grid's StyleTable
    /// and caches the resulting StyleId. Should be called after any SGR change.
    ///
    /// This is the Ghostty pattern: intern styles once when they change,
    /// then reuse the StyleId for all cells written with that style.
    #[inline]
    pub(super) fn update_style_id(&mut self) {
        // Fast path: SGR is now fully default (fg/bg/flags all default).
        // Skip ExtendedStyle construction and intern_extended entirely.
        if self.style.fg.is_default() && self.style.bg.is_default() && self.style.flags.is_empty() {
            // Refresh cached_colors: this path is reached not only via SGR 0
            // (reset_sgr, which already defaults the cache) but also when the
            // GENERIC loop drives fg/bg/flags back to default with individual
            // codes (e.g. `\x1b[39;49m`, `\x1b[31;42m\x1b[39;49m`). Those do NOT
            // call reset_sgr, so cached_colors would otherwise retain the prior
            // non-default colors and the next inline cell write (which reads
            // `cached_colors`, not the StyleId) would paint stale fg/bg.
            self.style.update_cached_colors();
            *self.current_style_id = StyleId::DEFAULT;
            // Reset BCE cursor template when SGR is fully default (#7522).
            self.grid
                .set_cursor_template(crate::grid::Cell::EMPTY, None);
            return;
        }

        self.style.update_cached_colors();
        // Update BCE cursor template from current SGR background (#7522).
        // This ensures line feeds, autowrap, and other scroll operations
        // that happen before the next explicit erase use the correct bg.
        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );
        // L1 probe: check if the Style matches the last interned style before
        // building the full ExtendedStyle. Avoids ExtendedStyle construction
        // and the non-inline intern_extended call on consecutive repeats (#7351).
        let style = self.style.build_style();
        if let Some(id) = self.grid.try_intern_style_l1(&style) {
            *self.current_style_id = id;
            return;
        }
        let ext_style = self.style.build_extended_style();
        *self.current_style_id = self.grid.intern_extended_style(ext_style);
    }

    /// Specialized style ID update after only the fg color changed.
    ///
    /// Skips the `cell_flags_to_attrs` loop and bg `unpack_color` conversion
    /// by reusing cached attrs and bg color from the previous style state.
    #[inline]
    pub(super) fn update_style_id_fg_changed(&mut self) {
        let ext_style = self.style.build_extended_style_fg_changed();
        if self.style.is_default() {
            *self.current_style_id = StyleId::DEFAULT;
        } else if let Some(id) = self.grid.try_intern_style_l1(&ext_style.style) {
            *self.current_style_id = id;
        } else {
            *self.current_style_id = self.grid.intern_extended_style(ext_style);
        }
    }

    /// Specialized style ID update after only attribute flag bits changed.
    ///
    /// Reuses the already-interned/cached fg and bg colors (and their types and
    /// palette indices) instead of rebuilding them via `build_extended_style()`,
    /// and skips `set_cursor_template` entirely: a flags-only change cannot alter
    /// the background, and the BCE cursor template depends only on bg (#7522), so
    /// the template set by the last bg-changing SGR is still correct. Mirrors the
    /// structure of `update_style_id_fg_changed` (#7351).
    #[inline]
    pub(super) fn update_style_id_flags_changed(&mut self) {
        self.style.update_flags_cache();
        if self.style.is_default() {
            // bg is necessarily default here, so the BCE template is already
            // EMPTY (set by whatever last made bg default) — nothing to reset.
            *self.current_style_id = StyleId::DEFAULT;
            return;
        }
        let style = self.style.build_style();
        if let Some(id) = self.grid.try_intern_style_l1(&style) {
            *self.current_style_id = id;
            return;
        }
        let ext_style = self.style.build_extended_style();
        *self.current_style_id = self.grid.intern_extended_style(ext_style);
    }

    /// Specialized style ID update after only the bg color changed.
    ///
    /// `old_bg` is the background color before this SGR was applied. When it is
    /// unchanged, the BCE cursor template (which depends only on bg) is already
    /// correct, so `set_cursor_template` is skipped (#7522).
    #[inline]
    pub(super) fn update_style_id_bg_changed(&mut self, old_bg: PackedColor) {
        let bg_changed = self.style.bg != old_bg;
        let ext_style = self.style.build_extended_style_bg_changed();
        // Update BCE cursor template from new bg only when bg actually changed.
        // PackedColor compares the full RGB value, so RGB→RGB changes are caught.
        if bg_changed {
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
        }
        if self.style.is_default() {
            *self.current_style_id = StyleId::DEFAULT;
        } else if let Some(id) = self.grid.try_intern_style_l1(&ext_style.style) {
            *self.current_style_id = id;
        } else {
            *self.current_style_id = self.grid.intern_extended_style(ext_style);
        }
    }

    /// Specialized style ID update after both fg and bg colors changed.
    #[inline]
    pub(super) fn update_style_id_both_changed(&mut self) {
        let ext_style = self.style.build_extended_style_both_changed();
        // Update BCE cursor template from new bg (#7522).
        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );
        if self.style.is_default() {
            *self.current_style_id = StyleId::DEFAULT;
        } else if let Some(id) = self.grid.try_intern_style_l1(&ext_style.style) {
            *self.current_style_id = id;
        } else {
            *self.current_style_id = self.grid.intern_extended_style(ext_style);
        }
    }

    /// Specialized style ID update after a leading attribute flag AND the fg
    /// color changed (e.g. `\x1b[1;38;5;202m`), with bg unchanged.
    ///
    /// Refreshes the attrs cache (flags changed) and the fg cache (fg changed)
    /// but keeps the bg cache, and skips `set_cursor_template`: bg is untouched
    /// so the BCE template from the last bg-changing SGR is still correct
    /// (#7522). Combines the work of `update_style_id_flags_changed` and
    /// `update_style_id_fg_changed` for the common `attr;38;5;N` TUI shape.
    #[inline]
    pub(super) fn update_style_id_flags_and_fg_changed(&mut self) {
        // Order matters: refresh cached_attrs first so build_extended_style_fg_changed
        // (which reuses cached_attrs) sees the new flag bits.
        self.style.update_flags_cache();
        let ext_style = self.style.build_extended_style_fg_changed();
        if self.style.is_default() {
            *self.current_style_id = StyleId::DEFAULT;
        } else if let Some(id) = self.grid.try_intern_style_l1(&ext_style.style) {
            *self.current_style_id = id;
        } else {
            *self.current_style_id = self.grid.intern_extended_style(ext_style);
        }
    }

    /// Specialized style ID update after a leading attribute flag AND the bg
    /// color changed (e.g. `\x1b[4;48;5;19m`), with fg unchanged.
    ///
    /// Refreshes the attrs cache (flags changed) and the bg cache (bg changed)
    /// but keeps the fg cache. Updates the BCE cursor template only when bg
    /// actually changed (#7522), mirroring `update_style_id_bg_changed`.
    #[inline]
    pub(super) fn update_style_id_flags_and_bg_changed(&mut self, old_bg: PackedColor) {
        self.style.update_flags_cache();
        let bg_changed = self.style.bg != old_bg;
        let ext_style = self.style.build_extended_style_bg_changed();
        if bg_changed {
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
        }
        if self.style.is_default() {
            *self.current_style_id = StyleId::DEFAULT;
        } else if let Some(id) = self.grid.try_intern_style_l1(&ext_style.style) {
            *self.current_style_id = id;
        } else {
            *self.current_style_id = self.grid.intern_extended_style(ext_style);
        }
    }

    /// Apply a single SGR parameter, returning the number of extra params consumed.
    ///
    /// Shared by both `handle_sgr` and `handle_sgr_with_subparams` to avoid
    /// duplicating the ~80-line match block. Returns extra params consumed
    /// (e.g., 4 for `38;2;r;g;b`) so the caller can advance the index.
    #[inline]
    fn apply_sgr_param(&mut self, params: &[u16], i: usize) -> usize {
        let param = params[i];
        match param {
            0 => {
                self.style.reset_sgr();
                self.transient.current_underline_color = None;
                self.transient.update_has_transient_extras();
            }
            1 => self.style.flags.insert(CellFlags::BOLD),
            2 => self.style.flags.insert(CellFlags::DIM),
            3 => self.style.flags.insert(CellFlags::ITALIC),
            4 => {
                self.style.flags.remove(CellFlags::ALL_UNDERLINES);
                self.style.flags.insert(CellFlags::UNDERLINE);
            }
            5 | 6 => self.style.flags.insert(CellFlags::BLINK),
            7 => self.style.flags.insert(CellFlags::INVERSE),
            8 => self.style.flags.insert(CellFlags::HIDDEN),
            9 => self.style.flags.insert(CellFlags::STRIKETHROUGH),
            21 => {
                self.style.flags.remove(CellFlags::ALL_UNDERLINES);
                self.style.flags.insert(CellFlags::DOUBLE_UNDERLINE);
            }
            22 => {
                self.style.flags.remove(CellFlags::BOLD);
                self.style.flags.remove(CellFlags::DIM);
            }
            23 => self.style.flags.remove(CellFlags::ITALIC),
            24 => self.style.flags.remove(CellFlags::ALL_UNDERLINES),
            25 => self.style.flags.remove(CellFlags::BLINK),
            27 => self.style.flags.remove(CellFlags::INVERSE),
            28 => self.style.flags.remove(CellFlags::HIDDEN),
            29 => self.style.flags.remove(CellFlags::STRIKETHROUGH),
            53 => {
                // Overline — mutually exclusive with superscript/subscript
                // (OVERLINE is encoded as SUPERSCRIPT | SUBSCRIPT)
                self.style.flags.remove(CellFlags::SUPERSCRIPT);
                self.style.flags.remove(CellFlags::SUBSCRIPT);
                self.style.flags.insert(CellFlags::OVERLINE);
            }
            55 => {
                // Only reset if actual overline state (both SUPERSCRIPT and
                // SUBSCRIPT bits set). OVERLINE is encoded as SUPERSCRIPT |
                // SUBSCRIPT; unconditional remove would clobber standalone
                // superscript or subscript.
                // Arm-local `if`, NOT a match guard: a `55 if .. =>` guard would
                // fall through to the `_ => self.update_style_id()` default when
                // false — a real behaviour change, so the collapse is unsound.
                #[allow(clippy::collapsible_match)]
                if self.style.flags.contains(CellFlags::OVERLINE) {
                    self.style.flags.remove(CellFlags::OVERLINE);
                }
            }
            73 => {
                // Superscript — clear subscript and overline first
                self.style.flags.remove(CellFlags::SUBSCRIPT);
                self.style.flags.remove(CellFlags::OVERLINE);
                self.style.flags.insert(CellFlags::SUPERSCRIPT);
            }
            74 => {
                // Subscript — clear superscript and overline first
                self.style.flags.remove(CellFlags::SUPERSCRIPT);
                self.style.flags.remove(CellFlags::OVERLINE);
                self.style.flags.insert(CellFlags::SUBSCRIPT);
            }
            75 => {
                // Reset superscript/subscript but preserve overline.
                // OVERLINE is encoded as SUPERSCRIPT | SUBSCRIPT, so
                // blindly removing both bits would clear overline too.
                // Arm-local `if`, NOT a match guard: a `75 if .. =>` guard would
                // fall through to the `_ => self.update_style_id()` default when
                // false — a real behaviour change, so the collapse is unsound.
                #[allow(clippy::collapsible_match)]
                if !self.style.flags.contains(CellFlags::OVERLINE) {
                    self.style.flags.remove(CellFlags::SUPERSCRIPT);
                    self.style.flags.remove(CellFlags::SUBSCRIPT);
                }
            }
            30..=37 => self.style.fg = PackedColor::indexed(sgr_color_u8(param - 30)),
            38 => {
                if let Some(color) = Self::parse_extended_color(&params[i..]) {
                    self.style.fg = color;
                    return Self::extended_color_skip(&params[i..]);
                }
            }
            39 => self.style.fg = PackedColor::DEFAULT_FG,
            40..=47 => self.style.bg = PackedColor::indexed(sgr_color_u8(param - 40)),
            48 => {
                if let Some(color) = Self::parse_extended_color(&params[i..]) {
                    self.style.bg = color;
                    return Self::extended_color_skip(&params[i..]);
                }
            }
            49 => self.style.bg = PackedColor::DEFAULT_BG,
            58 => {
                if let Some(color) = Self::parse_underline_color(&params[i..]) {
                    // Store raw parsed value (0x01_RRGGBB or 0x02_0000NN).
                    // Indexed colors are resolved at render time from the live
                    // palette so OSC 4 palette changes take effect (#7445).
                    self.transient.current_underline_color = Some(color);
                    self.transient.update_has_transient_extras();
                    return Self::extended_color_skip(&params[i..]);
                }
            }
            59 => {
                self.transient.current_underline_color = None;
                self.transient.update_has_transient_extras();
            }
            90..=97 => self.style.fg = PackedColor::indexed(sgr_color_u8(param - 90 + 8)),
            100..=107 => self.style.bg = PackedColor::indexed(sgr_color_u8(param - 100 + 8)),
            _ => {}
        }
        0
    }

    /// Return extra params to skip for extended color sequences.
    #[inline]
    fn extended_color_skip(params: &[u16]) -> usize {
        match params.get(1) {
            Some(&2) => 4, // 38;2;r;g;b
            Some(&5) => 2, // 38;5;n
            _ => 0,
        }
    }

    /// Handle SGR (Select Graphic Rendition) sequences.
    #[inline]
    #[allow(
        clippy::too_many_lines,
        reason = "sequential fast-path dispatch for the common SGR shapes before the generic loop"
    )]
    pub(super) fn handle_sgr(&mut self, params: &[u16]) {
        // Fast path: empty params means CSI m → same as CSI 0 m (SGR reset).
        // Must also clear underline color to match the CSI 0 m path (#7254).
        // Use reset_sgr() (not reset()) to preserve DECSCA protected attribute.
        if params.is_empty() {
            self.style.reset_sgr();
            self.transient.current_underline_color = None;
            self.transient.update_has_transient_extras();
            *self.current_style_id = StyleId::DEFAULT;
            self.grid
                .set_cursor_template(crate::grid::Cell::EMPTY, None);
            return;
        }

        // Fast path: CSI 0 m (SGR reset) — the most common SGR sequence.
        // After reset_sgr, style is always default, so skip the HashMap intern.
        if params.len() == 1 && params[0] == 0 {
            self.style.reset_sgr();
            self.transient.current_underline_color = None;
            self.transient.update_has_transient_extras();
            *self.current_style_id = StyleId::DEFAULT;
            self.grid
                .set_cursor_template(crate::grid::Cell::EMPTY, None);
            return;
        }

        // Fast path: single-param basic colors and attributes.
        // Covers the common case of ESC[32m, ESC[1m, etc. without loop overhead.
        // Color-only params use specialized intern to skip flags→attrs conversion.
        if params.len() == 1 {
            // Capture bg before apply so update_style_id_bg_changed can detect a
            // no-op bg change and skip set_cursor_template (#7522).
            let old_bg = self.style.bg;
            self.apply_sgr_param(params, 0);
            match params[0] {
                30..=37 | 90..=97 | 39 => self.update_style_id_fg_changed(),
                40..=47 | 100..=107 | 49 => self.update_style_id_bg_changed(old_bg),
                // Attribute flag-bit changes (bold/dim/italic/underline/blink/
                // reverse/hidden/strike + their reset forms, super/sub/overline).
                // These flip only flag bits, so reuse cached colors (#7351).
                1..=9 | 21..=25 | 27..=29 | 53 | 55 | 73..=75 => {
                    self.update_style_id_flags_changed();
                }
                _ => self.update_style_id(),
            }
            return;
        }

        // Fast path: 3-param 256-color fg (38;5;N) or bg (48;5;N).
        // Skips the while-loop and match dispatch for per-character palette cycling.
        // Uses specialized color-only intern to skip flags→attrs conversion.
        if params.len() == 3 && params[1] == 5 {
            let index = sgr_color_u8(params[2]);
            if params[0] == 38 {
                self.style.fg = PackedColor::indexed(index);
                self.update_style_id_fg_changed();
                return;
            }
            if params[0] == 48 {
                let old_bg = self.style.bg;
                self.style.bg = PackedColor::indexed(index);
                self.update_style_id_bg_changed(old_bg);
                return;
            }
        }

        // Fast path: 4-param attribute + 256-color (e.g. `\x1b[1;38;5;202m`,
        // `\x1b[4;48;5;19m`) — a leading attribute-flag SGR combined with a
        // 256-color fg/bg. This is the dominant shape in SGR-dense TUI output
        // yet falls through every existing fast path to the generic loop +
        // full `update_style_id`. params[0] is restricted to pure flag-toggle
        // SGRs (no color/reset/transient side effects), so exactly one colour
        // plus the flag bits change — routing to the combined specializations
        // avoids the loop dispatch and the redundant unchanged-colour rebuild.
        if params.len() == 4
            && params[2] == 5
            && matches!(params[0], 1..=9 | 21..=25 | 27..=29 | 53 | 55 | 73..=75)
        {
            if params[1] == 38 {
                self.apply_sgr_param(params, 0); // apply the attribute flag
                self.style.fg = PackedColor::indexed(sgr_color_u8(params[3]));
                self.update_style_id_flags_and_fg_changed();
                return;
            }
            if params[1] == 48 {
                let old_bg = self.style.bg;
                self.apply_sgr_param(params, 0); // apply the attribute flag
                self.style.bg = PackedColor::indexed(sgr_color_u8(params[3]));
                self.update_style_id_flags_and_bg_changed(old_bg);
                return;
            }
        }

        // Fast path: 5-param truecolor fg (38;2;R;G;B) or bg (48;2;R;G;B).
        // Skips the while-loop, match dispatch, parse_extended_color, and
        // extended_color_skip. Uses specialized color-only intern.
        if params.len() == 5 && params[1] == 2 {
            if params[0] == 38 {
                self.style.fg = PackedColor::rgb(
                    params[2].min(255) as u8,
                    params[3].min(255) as u8,
                    params[4].min(255) as u8,
                );
                self.update_style_id_fg_changed();
                return;
            }
            if params[0] == 48 {
                let old_bg = self.style.bg;
                self.style.bg = PackedColor::rgb(
                    params[2].min(255) as u8,
                    params[3].min(255) as u8,
                    params[4].min(255) as u8,
                );
                self.update_style_id_bg_changed(old_bg);
                return;
            }
        }

        // Fast path: 10-param combined truecolor fg+bg (38;2;R;G;B;48;2;R;G;B).
        // Common in modern terminals (bat, delta) — one CSI for both colors.
        if params.len() == 10
            && params[0] == 38
            && params[1] == 2
            && params[5] == 48
            && params[6] == 2
        {
            self.style.fg = PackedColor::rgb(
                params[2].min(255) as u8,
                params[3].min(255) as u8,
                params[4].min(255) as u8,
            );
            self.style.bg = PackedColor::rgb(
                params[7].min(255) as u8,
                params[8].min(255) as u8,
                params[9].min(255) as u8,
            );
            self.update_style_id_both_changed();
            return;
        }

        let mut i = 0;
        while i < params.len() {
            i += self.apply_sgr_param(params, i);
            i += 1;
        }

        self.update_style_id();
    }

    /// Handle SGR (Select Graphic Rendition) with subparameter support.
    ///
    /// This handles colon-separated subparameters like SGR 4:3 (curly underline).
    /// The subparam_mask indicates which params were preceded by a colon.
    #[inline]
    pub(super) fn handle_sgr_with_subparams(&mut self, params: &[u16], subparam_mask: u32) {
        // Empty params = CSI m → same as CSI 0 m. Clear underline color too (#7254).
        // Use reset_sgr() (not reset()) to preserve DECSCA protected attribute.
        if params.is_empty() {
            self.style.reset_sgr();
            self.transient.current_underline_color = None;
            self.transient.update_has_transient_extras();
            self.update_style_id();
            return;
        }

        let mut i = 0;
        while i < params.len() {
            let param = params[i];
            // subparam_mask is u32 — tracks the first 32 parameter positions
            // (MAX_PARAMS is 24). The `< 32` guard keeps the shift in range for
            // any caller-supplied slice length.
            let next_is_subparam =
                i + 1 < params.len() && i + 1 < 32 && (subparam_mask & (1u32 << (i + 1))) != 0;

            // Handle SGR 4 (underline) with subparameters
            if param == 4 && next_is_subparam {
                let subparam = params.get(i + 1).copied().unwrap_or(0);
                match subparam {
                    0 => self.style.flags.remove(CellFlags::ALL_UNDERLINES),
                    1 => {
                        self.style.flags.remove(CellFlags::ALL_UNDERLINES);
                        self.style.flags.insert(CellFlags::UNDERLINE);
                    }
                    2 => {
                        self.style.flags.remove(CellFlags::ALL_UNDERLINES);
                        self.style.flags.insert(CellFlags::DOUBLE_UNDERLINE);
                    }
                    3 => {
                        self.style.flags.remove(CellFlags::ALL_UNDERLINES);
                        self.style.flags.insert(CellFlags::CURLY_UNDERLINE);
                    }
                    4 => {
                        self.style.flags.remove(CellFlags::ALL_UNDERLINES);
                        self.style.flags.insert(CellFlags::DOTTED_UNDERLINE);
                    }
                    5 => {
                        self.style.flags.remove(CellFlags::ALL_UNDERLINES);
                        self.style.flags.insert(CellFlags::DASHED_UNDERLINE);
                    }
                    _ => {
                        self.style.flags.remove(CellFlags::ALL_UNDERLINES);
                        self.style.flags.insert(CellFlags::UNDERLINE);
                    }
                }
                i += 2;
                continue;
            }

            // Handle SGR 58 (underline color) with subparameters (ISO 8613-3 format)
            if param == 58 && next_is_subparam {
                // Compute colon-group size first so parse receives only the
                // colon-linked slice, not trailing semicolon params (#7253).
                let mut skip = 1;
                while i + skip < params.len()
                    && i + skip < 32
                    && (subparam_mask & (1u32 << (i + skip))) != 0
                {
                    skip += 1;
                }
                if let Some(color) = Self::parse_underline_color_colon(
                    &params[i..i + skip],
                    if i < 32 { subparam_mask >> i } else { 0 },
                ) {
                    // Store raw parsed value (0x01_RRGGBB or 0x02_0000NN).
                    // Indexed colors are resolved at render time from the live
                    // palette so OSC 4 palette changes take effect (#7445).
                    self.transient.current_underline_color = Some(color);
                    self.transient.update_has_transient_extras();
                }
                i += skip;
                continue;
            }

            // Handle SGR 38/48 (fg/bg color) with colon subparameters (ISO 8613-3)
            // Colon format: 38:2:cs:r:g:b or 38:5:n — has a colorspace param that
            // the semicolon path (parse_extended_color) doesn't account for (#7232).
            if (param == 38 || param == 48) && next_is_subparam {
                // Compute colon-group size first so parse receives only the
                // colon-linked slice, not trailing semicolon params (#7253).
                let mut skip = 1;
                while i + skip < params.len()
                    && i + skip < 32
                    && (subparam_mask & (1u32 << (i + skip))) != 0
                {
                    skip += 1;
                }
                if let Some(color) = Self::parse_extended_color_colon(&params[i..i + skip]) {
                    if param == 38 {
                        self.style.fg = color;
                    } else {
                        self.style.bg = color;
                    }
                }
                i += skip;
                continue;
            }

            // For all other parameters, use the shared SGR dispatch
            i += self.apply_sgr_param(params, i);
            i += 1;
        }

        self.update_style_id();
    }

    /// Parse extended color with colon subparameters (ISO 8613-3 format).
    ///
    /// Handles:
    /// - `38:5:Ps` / `48:5:Ps` — indexed color
    /// - `38:2:Pc:Pr:Pg:Pb` / `48:2:Pc:Pr:Pg:Pb` — RGB with colorspace
    /// - `38:2::Pr:Pg:Pb` / `48:2::Pr:Pg:Pb` — RGB with empty colorspace
    #[allow(
        clippy::cast_possible_truncation,
        reason = "values clamped to u8::MAX by .min()"
    )]
    fn parse_extended_color_colon(params: &[u16]) -> Option<PackedColor> {
        if params.len() < 3 {
            return None;
        }

        match params.get(1) {
            Some(&2) => {
                if params.len() >= 6 {
                    // Full format: 38:2:cs:r:g:b — skip colorspace at [2]
                    let r = params[3].min(u16::from(u8::MAX)) as u8;
                    let g = params[4].min(u16::from(u8::MAX)) as u8;
                    let b = params[5].min(u16::from(u8::MAX)) as u8;
                    Some(PackedColor::rgb(r, g, b))
                } else if params.len() >= 5 {
                    // Short format: 38:2:r:g:b (no colorspace)
                    let r = params[2].min(u16::from(u8::MAX)) as u8;
                    let g = params[3].min(u16::from(u8::MAX)) as u8;
                    let b = params[4].min(u16::from(u8::MAX)) as u8;
                    Some(PackedColor::rgb(r, g, b))
                } else {
                    None
                }
            }
            Some(&5) if params.len() >= 3 => {
                let index = params[2].min(u16::from(u8::MAX)) as u8;
                Some(PackedColor::indexed(index))
            }
            _ => None,
        }
    }

    /// Parse extended color (38;2;r;g;b or 38;5;n).
    #[allow(
        clippy::cast_possible_truncation,
        reason = "values clamped to u8::MAX by .min()"
    )]
    fn parse_extended_color(params: &[u16]) -> Option<PackedColor> {
        if params.len() < 2 {
            return None;
        }

        match params.get(1) {
            Some(&2) if params.len() >= 5 => {
                // True color: 38;2;r;g;b
                // .min(u8::MAX) clamps to [0, 255]; safe to truncate.
                let r = params[2].min(u16::from(u8::MAX)) as u8;
                let g = params[3].min(u16::from(u8::MAX)) as u8;
                let b = params[4].min(u16::from(u8::MAX)) as u8;
                Some(PackedColor::rgb(r, g, b))
            }
            Some(&5) if params.len() >= 3 => {
                // 256-color: 38;5;n — clamped to [0, 255].
                let index = params[2].min(u16::from(u8::MAX)) as u8;
                Some(PackedColor::indexed(index))
            }
            _ => None,
        }
    }

    /// Parse underline color (58;2;r;g;b or 58;5;n).
    ///
    /// Returns a u32 in format 0xTT_RRGGBB where:
    /// - TT = 0x01 for RGB color
    /// - TT = 0x02 for indexed color (index stored in low byte)
    fn parse_underline_color(params: &[u16]) -> Option<u32> {
        if params.len() < 2 {
            return None;
        }

        match params.get(1) {
            Some(&2) if params.len() >= 5 => {
                // True color: 58;2;r;g;b
                let r = u32::from(params[2].min(255));
                let g = u32::from(params[3].min(255));
                let b = u32::from(params[4].min(255));
                // Format: 0x01_RRGGBB (type=RGB)
                Some(0x01_000000 | (r << 16) | (g << 8) | b)
            }
            Some(&5) if params.len() >= 3 => {
                // 256-color: 58;5;n
                let index = u32::from(params[2].min(255));
                // Format: 0x02_0000NN (type=indexed)
                Some(0x02_000000 | index)
            }
            _ => None,
        }
    }

    /// Parse underline color with colon subparameters (ISO 8613-3 format).
    ///
    /// Handles:
    /// - 58:5:Ps - indexed color (params = [58, 5, index])
    /// - 58:2:Pc:Pr:Pg:Pb - RGB color (params = [58, 2, colorspace, r, g, b])
    /// - 58:2::Pr:Pg:Pb - RGB with empty colorspace (params = [58, 2, 0, r, g, b])
    ///
    /// The `subparam_mask` argument is shifted so bit `0` corresponds to params\[0\].
    fn parse_underline_color_colon(params: &[u16], _subparam_mask: u32) -> Option<u32> {
        if params.len() < 3 {
            return None;
        }

        match params.get(1) {
            Some(&2) => {
                // RGB color: 58:2:Pc:Pr:Pg:Pb or 58:2::Pr:Pg:Pb
                // Pc is the optional color space ID (we ignore it)
                // Check if we have enough params: at least 58, 2, cs, r, g, b (6 params)
                // or with implicit cs: 58, 2, r, g, b (5 params)
                if params.len() >= 6 {
                    // Full format: 58:2:cs:r:g:b
                    // Skip colorspace at params[2], use r/g/b at params[3..6]
                    let r = u32::from(params[3].min(255));
                    let g = u32::from(params[4].min(255));
                    let b = u32::from(params[5].min(255));
                    Some(0x01_000000 | (r << 16) | (g << 8) | b)
                } else if params.len() >= 5 {
                    // Short format without colorspace: 58:2:r:g:b
                    // (some terminals omit the colorspace entirely)
                    let r = u32::from(params[2].min(255));
                    let g = u32::from(params[3].min(255));
                    let b = u32::from(params[4].min(255));
                    Some(0x01_000000 | (r << 16) | (g << 8) | b)
                } else {
                    None
                }
            }
            Some(&5) if params.len() >= 3 => {
                // Indexed color: 58:5:Ps
                let index = u32::from(params[2].min(255));
                Some(0x02_000000 | index)
            }
            _ => None,
        }
    }
}
