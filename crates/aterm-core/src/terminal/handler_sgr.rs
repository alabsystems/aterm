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

use crate::grid::{CellFlags, PackedColor};

use super::handler::SgrStyleHandler;
use super::sgr_color_u8;

impl SgrStyleHandler<'_> {
    /// Apply an SGR rendition change: refresh the caches the cell writers read,
    /// and re-arm the BCE cursor template from the new background.
    ///
    /// WHAT THIS USED TO BE, AND WHY IT SHRANK. This was `update_style_id`, and
    /// its headline job was to intern the new rendition into the grid's
    /// `StyleTable`: a 4-way linear L1 scan, a 256-entry direct-mapped L2 probe
    /// (indexed) or L2b (RGB, keyed on `fg.r`), then an `FxHashMap` probe, then
    /// on miss a push to `styles`/`ref_counts`/`extended`/`lookup` plus a
    /// refcount bump at a near-random index of a Vec that grows to 65 535
    /// entries. The `StyleId` it produced was written to
    /// `Terminal::current_style_id` — and NOTHING read it. Putting a `StyleId`
    /// into a cell requires `CellFlags::USES_STYLE_ID`, and every writer of that
    /// bit is test-gated (`Row::write_char_with_style_id`, `Cell::with_style_id`,
    /// `Cell::set_style_id`); the production write path (`write_char_core`)
    /// reads `cached_colors()` and stores colours INLINE as `PackedColors`, with
    /// 24-bit overflow in the extras RGB ring. `Terminal::current_style_id()`
    /// was `#[cfg(test)]` and had no caller at all; checkpoint deliberately
    /// refused to carry the id. So the ladder was pure dead work on the hottest
    /// escape path, and the table it fed was a monotone leak in the bargain —
    /// `StyleTable::release()` has no production caller, so `compact()` can
    /// never reclaim, RIS does not clear the table, and it grew with distinct
    /// styles EVER seen up to a silent 65 535 cliff. Deleting the intern deletes
    /// both costs; nothing interns any more, so nothing grows.
    ///
    /// What survives is the part that was always load-bearing: the writer cache
    /// refresh (`update_cached_colors`, which feeds `cached_colors()` /
    /// `has_style_extras()` / `is_default()`) and the BCE cursor template
    /// (#7522), which makes line feeds, autowrap and scrolls that happen before
    /// the next explicit erase use the current background.
    #[inline]
    pub(super) fn apply_style_change(&mut self) {
        // Refreshing unconditionally also fixes the case the old fast path
        // documented: the GENERIC loop can drive fg/bg/flags back to default
        // with individual codes (`\x1b[39;49m`, `\x1b[31;42m\x1b[39;49m`)
        // without going through `reset_sgr`, and the next inline cell write
        // reads `cached_colors`, not a StyleId — so a skipped refresh paints
        // stale colours.
        self.style.update_cached_colors();
        // The old code split this into an "all default" arm that set
        // `Cell::EMPTY` + `None` and a general arm that set
        // `bce_blank(cached_colors)` + `bce_bg_rgb()`. The split existed only to
        // skip the intern; the two arms are the SAME VALUES. `Cell::bce_blank`
        // masks the bg out of the packed colours and returns `Cell::EMPTY`
        // verbatim when that mask is zero (a default background), and
        // `bce_bg_rgb()` is `None` unless `bg.is_rgb()` — which a default
        // background is not. One arm, one branch fewer per SGR.
        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );
    }

    /// Apply a rendition change that CANNOT have touched the background.
    ///
    /// Foreground-only and flags+foreground SGRs (`\x1b[31m`, `\x1b[38;5;Nm`,
    /// `\x1b[1;38;5;Nm`): the BCE cursor template depends only on bg (#7522), so
    /// the template set by the last bg-changing SGR is still correct and is left
    /// alone. Replaces `update_style_id_fg_changed` /
    /// `update_style_id_flags_and_fg_changed`, which differed from each other
    /// only in WHICH intern-feeding caches they rebuilt — a distinction that
    /// died with the interner.
    #[inline]
    pub(super) fn apply_style_change_keep_bce(&mut self) {
        self.style.update_cached_colors();
    }

    /// Apply a rendition change that may or may not have moved the background.
    ///
    /// `old_bg` is the background before this SGR. When it is unchanged the BCE
    /// cursor template is already correct and the store is skipped (#7522).
    /// `PackedColor` compares the full RGB value, so RGB→RGB changes are caught.
    #[inline]
    pub(super) fn apply_style_change_bg_maybe(&mut self, old_bg: PackedColor) {
        let bg_changed = self.style.bg != old_bg;
        self.style.update_cached_colors();
        if bg_changed {
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
        }
    }

    /// Apply an attribute-flag-only rendition change (`\x1b[1m`, `\x1b[22m`, …).
    ///
    /// Flag bits cannot alter `cached_colors` (a pure function of fg/bg), so this
    /// skips `convert_colors` and refreshes only the two flag-dependent booleans.
    /// It cannot alter the background either, so the BCE template stands.
    #[inline]
    pub(super) fn apply_flags_change(&mut self) {
        self.style.update_flags_cache();
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
                // fall through to the `_ => self.apply_style_change()` default when
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
                // fall through to the `_ => self.apply_style_change()` default when
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
            self.grid
                .set_cursor_template(crate::grid::Cell::EMPTY, None);
            return;
        }

        // Fast path: CSI 0 m (SGR reset) — the most common SGR sequence.
        // `reset_sgr` restores every cache to its default, so the template is
        // the only thing left to re-arm.
        if params.len() == 1 && params[0] == 0 {
            self.style.reset_sgr();
            self.transient.current_underline_color = None;
            self.transient.update_has_transient_extras();
            self.grid
                .set_cursor_template(crate::grid::Cell::EMPTY, None);
            return;
        }

        // Fast path: single-param basic colors and attributes.
        // Covers the common case of ESC[32m, ESC[1m, etc. without loop overhead.
        // Color-only params use specialized intern to skip flags→attrs conversion.
        if params.len() == 1 {
            // Capture bg before apply so apply_style_change_bg_maybe can detect a
            // no-op bg change and skip set_cursor_template (#7522).
            let old_bg = self.style.bg;
            self.apply_sgr_param(params, 0);
            match params[0] {
                30..=37 | 90..=97 | 39 => self.apply_style_change_keep_bce(),
                40..=47 | 100..=107 | 49 => self.apply_style_change_bg_maybe(old_bg),
                // Attribute flag-bit changes (bold/dim/italic/underline/blink/
                // reverse/hidden/strike + their reset forms, super/sub/overline).
                // These flip only flag bits, so reuse cached colors (#7351).
                1..=9 | 21..=25 | 27..=29 | 53 | 55 | 73..=75 => {
                    self.apply_flags_change();
                }
                _ => self.apply_style_change(),
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
                self.apply_style_change_keep_bce();
                return;
            }
            if params[0] == 48 {
                let old_bg = self.style.bg;
                self.style.bg = PackedColor::indexed(index);
                self.apply_style_change_bg_maybe(old_bg);
                return;
            }
        }

        // Fast path: 4-param attribute + 256-color (e.g. `\x1b[1;38;5;202m`,
        // `\x1b[4;48;5;19m`) — a leading attribute-flag SGR combined with a
        // 256-color fg/bg. This is the dominant shape in SGR-dense TUI output
        // yet falls through every existing fast path to the generic loop +
        // full `apply_style_change`. params[0] is restricted to pure flag-toggle
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
                self.apply_style_change_keep_bce();
                return;
            }
            if params[1] == 48 {
                let old_bg = self.style.bg;
                self.apply_sgr_param(params, 0); // apply the attribute flag
                self.style.bg = PackedColor::indexed(sgr_color_u8(params[3]));
                self.apply_style_change_bg_maybe(old_bg);
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
                self.apply_style_change_keep_bce();
                return;
            }
            if params[0] == 48 {
                let old_bg = self.style.bg;
                self.style.bg = PackedColor::rgb(
                    params[2].min(255) as u8,
                    params[3].min(255) as u8,
                    params[4].min(255) as u8,
                );
                self.apply_style_change_bg_maybe(old_bg);
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
            self.apply_style_change();
            return;
        }

        let mut i = 0;
        while i < params.len() {
            i += self.apply_sgr_param(params, i);
            i += 1;
        }

        self.apply_style_change();
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
            self.apply_style_change();
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

        self.apply_style_change();
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

#[cfg(test)]
mod tests {
    //! The semicolon-form underline colour (SGR `58;2;r;g;b` / `58;5;n`),
    //! pinned at the byte level.
    //!
    //! Audit 2026-09-22 (Windows): `ESC[4m ESC[58;2;255;0;0m TEXT` underlined in
    //! the default grey, `ESC[58;5;196m` likewise, and `ESC[4m` followed by
    //! `ESC[58;2;0;255;0m` switched the underline OFF — the shape you get when
    //! `58;2;0;255;0` is read as five independent params (2 = dim, 0 = reset),
    //! while the colon form `58:2::255:0:0` was fine. These tests feed EXACTLY
    //! those byte sequences to the terminal and read the grid back, so a
    //! regression in the parser's parameter consumption or in the SGR dispatch
    //! fails HERE, in the crate that owns it, rather than in a screenshot.
    //!
    //! They pass with no change to the SGR code (0.90.0 and 0.94.0 alike): aterm
    //! was never the layer that dropped the colour. MEASURED on the host that
    //! runs every Windows tab (the inbox conhost 10.0.26100.1 behind
    //! `CreatePseudoConsole` with `dwFlags` 0, Windows 10.0.26200) by recording
    //! the pseudoconsole's output pipe while a child wrote the sequences:
    //! `ESC[4m ESC[58;5;196m` came back as `ESC[4m ESC[5m` (the index became
    //! BLINK), `ESC[4m ESC[58;2;1;1;1m` as `ESC[1m ESC[2m ESC[4m` (bold, dim),
    //! and `ESC[4m ESC[58;2;255;0;0m`, `ESC[4m ESC[58;2;0;255;0m` and the
    //! combined `ESC[4;58;2;255;0;0m` as no rendition at all (the trailing `0`
    //! was SGR 0); the grey underline is the colour-first order
    //! `ESC[58;2;255;0;0m ESC[4m`, which came back as a bare `ESC[4m`.
    //! `ESC[58:2::255:0:0m`, `ESC[58:5:196m` and `ESC[38;2;255;0;0m` came back
    //! verbatim, while the colon form WITHOUT the colour-space slot,
    //! `ESC[58:2:255:0:0m`, was dropped. conhost's `_ApplyGraphicsOption` has no
    //! `UnderlineColor` arm — `58` takes the `default: return 1` path and its
    //! sub-values are dispatched as their own SGRs; only the colon path
    //! (`_ApplyGraphicsOptionWithSubParams`) reads a colour for 58, and the same
    //! holds at microsoft/terminal `main` (read 2026-09-27). Nothing in aterm
    //! can restore bytes conhost never forwards; on Windows a program wanting a
    //! coloured underline must send `58:2::r:g:b` or `58:5:n`.

    use super::super::Terminal;
    use super::super::render_cells::UnderlineStyle;
    use super::*;

    /// `0x01_RRGGBB`: the packed form `parse_underline_color` stores for RGB.
    const RED: u32 = 0x01_FF00_00;
    const GREEN: u32 = 0x01_00FF_00;
    /// `0x02_0000NN`: the packed form for a palette index.
    const IDX_196: u32 = 0x02_0000_C4;

    type Drawn = Vec<(char, UnderlineStyle, Option<[u8; 3]>)>;

    /// `(char, underline style, resolved underline colour)` for the first `n`
    /// cells of row 0 — what the renderer will draw.
    fn drawn(term: &mut Terminal, n: usize) -> Drawn {
        term.render_row(0)
            .iter()
            .take(n)
            .map(|c| (c.ch, c.underline, c.underline_color))
            .collect()
    }

    fn single(text: &str, color: [u8; 3]) -> Drawn {
        text.chars()
            .map(|ch| (ch, UnderlineStyle::Single, Some(color)))
            .collect()
    }

    #[test]
    fn semicolon_rgb_after_sgr_4_colours_the_underline_red() {
        let mut term = Terminal::new(2, 16);
        term.process(b"\x1b[4m\x1b[58;2;255;0;0m");
        assert_eq!(
            term.transient.current_underline_color,
            Some(RED),
            "58;2;255;0;0 is one RGB colour"
        );
        assert!(
            term.style.flags.contains(CellFlags::UNDERLINE),
            "SGR 4 survives the colour"
        );
        term.process(b"TEXT\x1b[0m");
        assert_eq!(drawn(&mut term, 4), single("TEXT", [255, 0, 0]));
        assert_eq!(
            term.transient.current_underline_color, None,
            "SGR 0 clears the colour"
        );
    }

    #[test]
    fn semicolon_indexed_after_sgr_4_colours_the_underline_from_the_palette() {
        let mut term = Terminal::new(2, 16);
        term.process(b"\x1b[4m\x1b[58;5;196m");
        assert_eq!(
            term.transient.current_underline_color,
            Some(IDX_196),
            "58;5;196 is one palette index"
        );
        assert!(term.style.flags.contains(CellFlags::UNDERLINE));
        assert!(
            !term.style.flags.contains(CellFlags::BLINK),
            "5 is the colour form, not SGR 5"
        );
        term.process(b"TEXT\x1b[0m");
        let c = term.color_palette().get(196);
        assert_eq!(drawn(&mut term, 4), single("TEXT", [c.r, c.g, c.b]));
    }

    #[test]
    fn semicolon_rgb_with_zero_components_keeps_the_underline_on() {
        // The audit's tell: `58;2;0;255;0` read as five SEPARATE params would
        // apply 2 (dim) and 0 (full reset) and leave no underline at all.
        let mut term = Terminal::new(2, 16);
        term.process(b"\x1b[4m\x1b[58;2;0;255;0m");
        assert_eq!(term.transient.current_underline_color, Some(GREEN));
        assert!(
            term.style.flags.contains(CellFlags::UNDERLINE),
            "the underline must survive its own colour"
        );
        assert!(
            !term.style.flags.contains(CellFlags::DIM),
            "2 is the colour space, not SGR 2"
        );
        assert!(
            term.style.fg == PackedColor::DEFAULT_FG && term.style.bg == PackedColor::DEFAULT_BG,
            "no component of the triple is a reset"
        );
        term.process(b"TEXT\x1b[0m");
        assert_eq!(drawn(&mut term, 4), single("TEXT", [0, 255, 0]));
    }

    #[test]
    fn semicolon_colour_split_at_every_read_boundary_is_one_colour() {
        // The pipe read can end anywhere inside the CSI (a ConPTY read returns
        // whatever conhost has flushed), so the params must survive a split
        // at every byte: one byte per `process` is the worst case of it.
        for (bytes, want) in [
            (&b"\x1b[4m\x1b[58;2;0;255;0mTEXT\x1b[0m"[..], [0, 255, 0]),
            (&b"\x1b[4;58;2;255;0;0mTEXT\x1b[0m"[..], [255, 0, 0]),
        ] {
            let mut term = Terminal::new(2, 16);
            for b in bytes {
                term.process(std::slice::from_ref(b));
            }
            assert_eq!(
                drawn(&mut term, 4),
                single("TEXT", want),
                "{:?} fed one byte at a time",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn colon_semicolon_and_combined_forms_draw_the_same_cells() {
        let mut semicolon = Terminal::new(2, 16);
        semicolon.process(b"\x1b[4m\x1b[58;2;255;0;0mTEXT\x1b[0m");
        let mut colon = Terminal::new(2, 16);
        colon.process(b"\x1b[58:2::255:0:0m\x1b[4mTEXT\x1b[0m");
        let mut combined = Terminal::new(2, 16);
        combined.process(b"\x1b[4;58;2;255;0;0mTEXT\x1b[0m");
        let want = single("TEXT", [255, 0, 0]);
        assert_eq!(drawn(&mut semicolon, 4), want, "58;2;r;g;b");
        assert_eq!(drawn(&mut colon, 4), want, "58:2::r:g:b");
        assert_eq!(drawn(&mut combined, 4), want, "4;58;2;r;g;b");
    }

    #[test]
    fn sgr_59_clears_the_colour_but_not_the_underline() {
        let mut term = Terminal::new(2, 16);
        term.process(b"\x1b[4m\x1b[58;2;255;0;0mA\x1b[59m");
        assert_eq!(
            term.transient.current_underline_color, None,
            "59 resets the colour"
        );
        assert!(
            term.style.flags.contains(CellFlags::UNDERLINE),
            "59 leaves the underline itself alone"
        );
        term.process(b"B\x1b[0m");
        assert_eq!(
            drawn(&mut term, 2),
            vec![
                ('A', UnderlineStyle::Single, Some([255, 0, 0])),
                // No SGR 58 colour: the line takes the cell's own ink.
                ('B', UnderlineStyle::Single, None),
            ]
        );
    }

    #[test]
    fn a_param_after_the_semicolon_colour_is_applied_on_its_own() {
        // Exact consumption at the byte level: the param AFTER the triple or
        // the index is dispatched as its own SGR — neither swallowed by the
        // colour nor the colour's tail read as attributes.
        let mut term = Terminal::new(2, 16);
        term.process(b"\x1b[58;2;255;0;0;1m");
        assert_eq!(term.transient.current_underline_color, Some(RED));
        assert!(term.style.flags.contains(CellFlags::BOLD), "58;2;r;g;b;1");
        term.process(b"\x1b[0m\x1b[58;5;196;3m");
        assert_eq!(term.transient.current_underline_color, Some(IDX_196));
        assert!(term.style.flags.contains(CellFlags::ITALIC), "58;5;n;3");
        // 38 and 48 must agree with 58 on the same shapes.
        term.process(b"\x1b[0m\x1b[38;2;1;2;3;4m");
        assert!(term.style.fg == PackedColor::rgb(1, 2, 3), "38;2;r;g;b");
        assert!(
            term.style.flags.contains(CellFlags::UNDERLINE),
            "38;2;r;g;b;4"
        );
        term.process(b"\x1b[0m\x1b[48;5;7;9m");
        assert!(term.style.bg == PackedColor::indexed(7), "48;5;n");
        assert!(
            term.style.flags.contains(CellFlags::STRIKETHROUGH),
            "48;5;n;9"
        );
        // And an underline colour followed by a colour reset keeps the
        // underline colour: 39 is the foreground's, not 58's.
        term.process(b"\x1b[0m\x1b[4;58;2;255;0;0;39m");
        assert_eq!(term.transient.current_underline_color, Some(RED));
        assert!(term.style.fg == PackedColor::DEFAULT_FG, "4;58;2;r;g;b;39");
    }

    #[test]
    fn semicolon_forms_consume_exactly_their_sub_parameters() {
        // The parse and the skip are two functions; they must agree on the
        // width of each form: 58;2;r;g;b is FIVE params (four after the 58),
        // 58;5;n is THREE (two after).
        assert_eq!(
            SgrStyleHandler::parse_underline_color(&[58, 2, 255, 0, 0]),
            Some(RED)
        );
        assert_eq!(SgrStyleHandler::extended_color_skip(&[58, 2, 255, 0, 0]), 4);
        assert_eq!(
            SgrStyleHandler::parse_underline_color(&[58, 5, 196]),
            Some(IDX_196)
        );
        assert_eq!(SgrStyleHandler::extended_color_skip(&[58, 5, 196]), 2);
        // A truncated triple is refused, never read short.
        assert_eq!(
            SgrStyleHandler::parse_underline_color(&[58, 2, 255, 0]),
            None
        );
        // 38/48 use the same skip and the same widths.
        assert!(
            SgrStyleHandler::parse_extended_color(&[38, 2, 1, 2, 3])
                == Some(PackedColor::rgb(1, 2, 3))
        );
        assert_eq!(SgrStyleHandler::extended_color_skip(&[38, 2, 1, 2, 3]), 4);
        assert!(
            SgrStyleHandler::parse_extended_color(&[48, 5, 7]) == Some(PackedColor::indexed(7))
        );
        assert_eq!(SgrStyleHandler::extended_color_skip(&[48, 5, 7]), 2);
        assert!(SgrStyleHandler::parse_extended_color(&[38, 2, 1, 2]).is_none());
    }
}
