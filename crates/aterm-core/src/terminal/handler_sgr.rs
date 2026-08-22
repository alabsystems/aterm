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
