// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! SGR (Select Graphic Rendition) handler: text attributes, 8/16/256/true
//! colour foreground and background, underline colours and styles,
//! superscript/subscript, and colon subparameters (ISO 8613-3).

use crate::grid::{CellFlags, PackedColor};

use super::handler::SgrStyleHandler;
use super::sgr_color_u8;

impl SgrStyleHandler<'_> {
    /// Apply an SGR rendition change: refresh the writer caches
    /// (`update_cached_colors`) and re-arm the BCE cursor template from the new
    /// background, so line feeds, autowrap and scrolls before the next explicit
    /// erase use it.
    #[inline]
    pub(super) fn apply_style_change(&mut self) {
        // Refresh unconditionally: individual codes (`\x1b[39;49m`) can return
        // fg/bg/flags to default without `reset_sgr`, and the next cell write
        // reads `cached_colors`.
        self.style.update_cached_colors();
        // One arm covers the default case too: `Cell::bce_blank` returns
        // `Cell::EMPTY` for a default background, and `bce_bg_rgb()` is `None`
        // unless the background is RGB.
        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );
    }

    /// Apply a rendition change that cannot have touched the background
    /// (`\x1b[31m`, `\x1b[38;5;Nm`, `\x1b[1;38;5;Nm`). The BCE cursor template
    /// depends only on bg, so it is left alone.
    #[inline]
    pub(super) fn apply_style_change_keep_bce(&mut self) {
        self.style.update_cached_colors();
    }

    /// Apply a rendition change that may have moved the background. `old_bg` is
    /// the background before this SGR; when unchanged the BCE template store is
    /// skipped. `PackedColor` compares full RGB values.
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

    /// Apply a single SGR parameter, returning how many extra params it consumed
    /// (e.g. 4 for `38;2;r;g;b`). Shared by `handle_sgr` and
    /// `handle_sgr_with_subparams`.
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
                // OVERLINE is encoded as SUPERSCRIPT | SUBSCRIPT, so overline,
                // superscript and subscript exclude each other.
                self.style.flags.remove(CellFlags::SUPERSCRIPT);
                self.style.flags.remove(CellFlags::SUBSCRIPT);
                self.style.flags.insert(CellFlags::OVERLINE);
            }
            55 => {
                // Reset only a real overline (both bits set); a lone superscript
                // or subscript stays. An arm-local `if`, not a match guard, so a
                // false test never falls through to a later arm.
                #[allow(clippy::collapsible_match)]
                if self.style.flags.contains(CellFlags::OVERLINE) {
                    self.style.flags.remove(CellFlags::OVERLINE);
                }
            }
            73 => {
                self.style.flags.remove(CellFlags::SUBSCRIPT);
                self.style.flags.remove(CellFlags::OVERLINE);
                self.style.flags.insert(CellFlags::SUPERSCRIPT);
            }
            74 => {
                self.style.flags.remove(CellFlags::SUPERSCRIPT);
                self.style.flags.remove(CellFlags::OVERLINE);
                self.style.flags.insert(CellFlags::SUBSCRIPT);
            }
            75 => {
                // Keep a real overline (both bits). Arm-local `if`, as for 55.
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
                    // The raw packed value (0x01_RRGGBB or 0x02_0000NN): an index
                    // resolves at render time from the live palette, so OSC 4
                    // changes apply.
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
        // Fast path: empty params (CSI m) is SGR 0. Also clear the underline
        // colour, and use reset_sgr() so DECSCA protection is kept.
        if params.is_empty() {
            self.style.reset_sgr();
            self.transient.current_underline_color = None;
            self.transient.update_has_transient_extras();
            self.grid
                .set_cursor_template(crate::grid::Cell::EMPTY, None);
            return;
        }

        // Fast path: CSI 0 m, the most common SGR. `reset_sgr` restores every
        // cache, so only the template needs re-arming.
        if params.len() == 1 && params[0] == 0 {
            self.style.reset_sgr();
            self.transient.current_underline_color = None;
            self.transient.update_has_transient_extras();
            self.grid
                .set_cursor_template(crate::grid::Cell::EMPTY, None);
            return;
        }

        // Fast path: single-param basic colours and attributes (ESC[32m, ESC[1m)
        // without the loop.
        if params.len() == 1 {
            let old_bg = self.style.bg;
            self.apply_sgr_param(params, 0);
            match params[0] {
                30..=37 | 90..=97 | 39 => self.apply_style_change_keep_bce(),
                40..=47 | 100..=107 | 49 => self.apply_style_change_bg_maybe(old_bg),
                // Flag-only codes reuse the cached colours.
                1..=9 | 21..=25 | 27..=29 | 53 | 55 | 73..=75 => {
                    self.apply_flags_change();
                }
                _ => self.apply_style_change(),
            }
            return;
        }

        // Fast path: 3-param 256-colour fg (38;5;N) or bg (48;5;N), skipping the
        // loop and match dispatch.
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

        // Fast path: 4-param attribute plus 256-colour (`\x1b[1;38;5;202m`,
        // `\x1b[4;48;5;19m`), common in SGR-dense TUI output. params[0] is limited
        // to pure flag toggles, so exactly one colour and the flag bits change.
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

        // Fast path: 5-param truecolour fg (38;2;R;G;B) or bg (48;2;R;G;B),
        // skipping the loop, match dispatch and parse_extended_color.
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

        // Fast path: 10-param truecolor fg+bg (38;2;R;G;B;48;2;R;G;B), one CSI for
        // both colors (bat, delta).
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

    /// Handle SGR with colon subparameters (e.g. SGR 4:3, curly underline).
    /// `subparam_mask` marks which params were preceded by a colon.
    #[inline]
    pub(super) fn handle_sgr_with_subparams(&mut self, params: &[u16], subparam_mask: u32) {
        // CSI m is SGR 0; reset_sgr() keeps DECSCA protection.
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
            // subparam_mask covers the first 32 positions (MAX_PARAMS is 24); the
            // `< 32` guard keeps the shift in range for any slice length.
            let next_is_subparam =
                i + 1 < params.len() && i + 1 < 32 && (subparam_mask & (1u32 << (i + 1))) != 0;

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

            // SGR 58 (underline color) with colon subparameters.
            if param == 58 && next_is_subparam {
                // Compute colon-group size first so parse receives only the
                // colon-linked slice, not trailing semicolon params.
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
                    // The raw packed value, as in the semicolon form.
                    self.transient.current_underline_color = Some(color);
                    self.transient.update_has_transient_extras();
                }
                i += skip;
                continue;
            }

            // SGR 38/48 with colon subparameters (ISO 8613-3): 38:2:cs:r:g:b or
            // 38:5:n, including the colourspace slot the semicolon path lacks.
            if (param == 38 || param == 48) && next_is_subparam {
                // Only the colon group, as for 58.
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
                Some(0x01_000000 | (r << 16) | (g << 8) | b)
            }
            Some(&5) if params.len() >= 3 => {
                // 256-color: 58;5;n
                let index = u32::from(params[2].min(255));
                Some(0x02_000000 | index)
            }
            _ => None,
        }
    }

    /// Parse underline color with colon subparameters (ISO 8613-3), `params`
    /// being only the colon group:
    /// - `58:5:Ps`: indexed
    /// - `58:2:Pc:Pr:Pg:Pb`: RGB with a colour space
    /// - `58:2::Pr:Pg:Pb`: RGB with an empty colour space (read as 0)
    ///
    /// `_subparam_mask` (bit 0 = `params[0]`) is unused.
    fn parse_underline_color_colon(params: &[u16], _subparam_mask: u32) -> Option<u32> {
        if params.len() < 3 {
            return None;
        }

        match params.get(1) {
            Some(&2) => {
                // RGB: 58:2:Pc:Pr:Pg:Pb or 58:2::Pr:Pg:Pb. Pc (colour space) is
                // ignored; 6 params with it, 5 without.
                if params.len() >= 6 {
                    let r = u32::from(params[3].min(255));
                    let g = u32::from(params[4].min(255));
                    let b = u32::from(params[5].min(255));
                    Some(0x01_000000 | (r << 16) | (g << 8) | b)
                } else if params.len() >= 5 {
                    let r = u32::from(params[2].min(255));
                    let g = u32::from(params[3].min(255));
                    let b = u32::from(params[4].min(255));
                    Some(0x01_000000 | (r << 16) | (g << 8) | b)
                } else {
                    None
                }
            }
            Some(&5) if params.len() >= 3 => {
                let index = u32::from(params[2].min(255));
                Some(0x02_000000 | index)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    //! The semicolon-form underline colour (`58;2;r;g;b` / `58;5;n`), fed as
    //! bytes and read back from the grid.
    //!
    //! conhost does not forward the semicolon form (its `_ApplyGraphicsOption`
    //! has no `UnderlineColor` arm, so `58;5;196` arrives as separate SGRs); under
    //! ConPTY only the colon forms `58:2::r:g:b` and `58:5:n` pass through.

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
        // Read as five separate params, `58;2;0;255;0` would apply 2 (dim) and
        // 0 (reset) and leave no underline.
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
        // A ConPTY read can end anywhere inside the CSI, so the params must
        // survive a split at every byte; one byte per `process` is the worst
        // case.
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
        // Exact consumption: the param after the triple or index is its own SGR,
        // neither swallowed by the colour nor the colour's tail read as
        // attributes.
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
        // Parse and skip must agree on each form's width: 58;2;r;g;b is five
        // params (four after 58), 58;5;n is three (two after).
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
