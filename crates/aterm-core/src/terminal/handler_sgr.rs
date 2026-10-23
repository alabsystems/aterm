// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! SGR (Select Graphic Rendition) handler.

use crate::grid::{CellFlags, PackedColor};

use super::handler::SgrStyleHandler;
use super::sgr_color_u8;

impl SgrStyleHandler<'_> {
    /// Apply an SGR rendition change.
    #[inline]
    pub(super) const fn apply_style_change(&mut self) {
        self.style.update_cached_colors();
        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );
    }

    /// Apply a rendition change that does not modify the background color.
    #[inline]
    pub(super) const fn apply_style_change_keep_bce(&mut self) {
        self.style.update_cached_colors();
    }

    /// Apply a rendition change that may have moved the background.
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
    #[inline]
    pub(super) const fn apply_flags_change(&mut self) {
        self.style.update_flags_cache();
    }

    /// Apply a single SGR parameter, returning how many extra parameters it consumed.
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
                self.style.flags.remove(CellFlags::SUPERSCRIPT);
                self.style.flags.remove(CellFlags::SUBSCRIPT);
                self.style.flags.insert(CellFlags::OVERLINE);
            }
            55 =>
            {
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
            75 =>
            {
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
                }
                return Self::extended_color_skip(&params[i..]);
            }
            39 => self.style.fg = PackedColor::DEFAULT_FG,
            40..=47 => self.style.bg = PackedColor::indexed(sgr_color_u8(param - 40)),
            48 => {
                if let Some(color) = Self::parse_extended_color(&params[i..]) {
                    self.style.bg = color;
                }
                return Self::extended_color_skip(&params[i..]);
            }
            49 => self.style.bg = PackedColor::DEFAULT_BG,
            58 => {
                if let Some(color) = Self::parse_underline_color(&params[i..]) {
                    self.transient.current_underline_color = Some(color);
                    self.transient.update_has_transient_extras();
                }
                return Self::extended_color_skip(&params[i..]);
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

    /// Extra parameters consumed by an extended color code (`38`, `48`, `58`).
    #[inline]
    const fn extended_color_skip(params: &[u16]) -> usize {
        if params.len() < 2 {
            return 0; // a bare 38 at the end of the list
        }
        match params[1] {
            2 => 4, // 38;2;r;g;b
            5 => 2, // 38;5;n
            _ => 1, // an unknown selector consumes itself
        }
    }

    /// Handle SGR (Select Graphic Rendition) sequences.
    #[inline]
    #[allow(
        clippy::too_many_lines,
        reason = "sequential fast-path dispatch for the common SGR shapes before the generic loop"
    )]
    pub(super) fn handle_sgr(&mut self, params: &[u16]) {
        if params.is_empty() {
            self.style.reset_sgr();
            self.transient.current_underline_color = None;
            self.transient.update_has_transient_extras();
            self.grid
                .set_cursor_template(crate::grid::Cell::EMPTY, None);
            return;
        }

        if params.len() == 1 && params[0] == 0 {
            self.style.reset_sgr();
            self.transient.current_underline_color = None;
            self.transient.update_has_transient_extras();
            self.grid
                .set_cursor_template(crate::grid::Cell::EMPTY, None);
            return;
        }

        if params.len() == 1 {
            let old_bg = self.style.bg;
            self.apply_sgr_param(params, 0);
            match params[0] {
                30..=37 | 90..=97 | 39 => self.apply_style_change_keep_bce(),
                40..=47 | 100..=107 | 49 => self.apply_style_change_bg_maybe(old_bg),
                1..=9 | 21..=25 | 27..=29 | 53 | 55 | 73..=75 => {
                    self.apply_flags_change();
                }
                _ => self.apply_style_change(),
            }
            return;
        }

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

    /// Handle SGR with colon subparameters.
    #[inline]
    pub(super) fn handle_sgr_with_subparams(&mut self, params: &[u16], subparam_mask: u32) {
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

            if param == 58 && next_is_subparam {
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
                    self.transient.current_underline_color = Some(color);
                    self.transient.update_has_transient_extras();
                }
                i += skip;
                continue;
            }

            if (param == 38 || param == 48) && next_is_subparam {
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
    #[allow(
        clippy::cast_possible_truncation,
        reason = "values clamped to u8::MAX by .min()"
    )]
    #[inline]
    const fn parse_extended_color_colon(params: &[u16]) -> Option<PackedColor> {
        if params.len() < 3 {
            return None;
        }

        match params[1] {
            2 => {
                if params.len() >= 6 {
                    let r = if params[3] > 255 {
                        255
                    } else {
                        params[3] as u8
                    };
                    let g = if params[4] > 255 {
                        255
                    } else {
                        params[4] as u8
                    };
                    let b = if params[5] > 255 {
                        255
                    } else {
                        params[5] as u8
                    };
                    Some(PackedColor::rgb(r, g, b))
                } else if params.len() >= 5 {
                    let r = if params[2] > 255 {
                        255
                    } else {
                        params[2] as u8
                    };
                    let g = if params[3] > 255 {
                        255
                    } else {
                        params[3] as u8
                    };
                    let b = if params[4] > 255 {
                        255
                    } else {
                        params[4] as u8
                    };
                    Some(PackedColor::rgb(r, g, b))
                } else {
                    None
                }
            }
            5 => {
                let index = if params[2] > 255 {
                    255
                } else {
                    params[2] as u8
                };
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
    #[inline]
    const fn parse_extended_color(params: &[u16]) -> Option<PackedColor> {
        if params.len() < 2 {
            return None;
        }

        match params[1] {
            2 if params.len() >= 5 => {
                let r = if params[2] > 255 {
                    255
                } else {
                    params[2] as u8
                };
                let g = if params[3] > 255 {
                    255
                } else {
                    params[3] as u8
                };
                let b = if params[4] > 255 {
                    255
                } else {
                    params[4] as u8
                };
                Some(PackedColor::rgb(r, g, b))
            }
            5 if params.len() >= 3 => {
                let index = if params[2] > 255 {
                    255
                } else {
                    params[2] as u8
                };
                Some(PackedColor::indexed(index))
            }
            _ => None,
        }
    }

    /// Parse underline color (58;2;r;g;b or 58;5;n).
    #[inline]
    const fn parse_underline_color(params: &[u16]) -> Option<u32> {
        if params.len() < 2 {
            return None;
        }

        match params[1] {
            2 if params.len() >= 5 => {
                let r = if params[2] > 255 {
                    255
                } else {
                    params[2] as u32
                };
                let g = if params[3] > 255 {
                    255
                } else {
                    params[3] as u32
                };
                let b = if params[4] > 255 {
                    255
                } else {
                    params[4] as u32
                };
                Some(0x01_000000 | (r << 16) | (g << 8) | b)
            }
            5 if params.len() >= 3 => {
                let index = if params[2] > 255 {
                    255
                } else {
                    params[2] as u32
                };
                Some(0x02_000000 | index)
            }
            _ => None,
        }
    }

    /// Parse an underline color from colon subparameters (ISO 8613-3).
    #[inline]
    const fn parse_underline_color_colon(params: &[u16], _subparam_mask: u32) -> Option<u32> {
        if params.len() < 3 {
            return None;
        }

        match params[1] {
            2 => {
                if params.len() >= 6 {
                    let r = if params[3] > 255 {
                        255
                    } else {
                        params[3] as u32
                    };
                    let g = if params[4] > 255 {
                        255
                    } else {
                        params[4] as u32
                    };
                    let b = if params[5] > 255 {
                        255
                    } else {
                        params[5] as u32
                    };
                    Some(0x01_000000 | (r << 16) | (g << 8) | b)
                } else if params.len() >= 5 {
                    let r = if params[2] > 255 {
                        255
                    } else {
                        params[2] as u32
                    };
                    let g = if params[3] > 255 {
                        255
                    } else {
                        params[3] as u32
                    };
                    let b = if params[4] > 255 {
                        255
                    } else {
                        params[4] as u32
                    };
                    Some(0x01_000000 | (r << 16) | (g << 8) | b)
                } else {
                    None
                }
            }
            5 => {
                let index = if params[2] > 255 {
                    255
                } else {
                    params[2] as u32
                };
                Some(0x02_000000 | index)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {

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

    /// `(char, underline style, resolved underline colour)` for the first `n` cells of row 0.
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
                ('B', UnderlineStyle::Single, None),
            ]
        );
    }

    #[test]
    fn a_param_after_the_semicolon_colour_is_applied_on_its_own() {
        let mut term = Terminal::new(2, 16);
        term.process(b"\x1b[58;2;255;0;0;1m");
        assert_eq!(term.transient.current_underline_color, Some(RED));
        assert!(term.style.flags.contains(CellFlags::BOLD), "58;2;r;g;b;1");
        term.process(b"\x1b[0m\x1b[58;5;196;3m");
        assert_eq!(term.transient.current_underline_color, Some(IDX_196));
        assert!(term.style.flags.contains(CellFlags::ITALIC), "58;5;n;3");
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
        term.process(b"\x1b[0m\x1b[4;58;2;255;0;0;39m");
        assert_eq!(term.transient.current_underline_color, Some(RED));
        assert!(term.style.fg == PackedColor::DEFAULT_FG, "4;58;2;r;g;b;39");
    }

    /// A colour the engine refuses still consumes what xterm's
    /// `parse_extended_colors` consumes: the selector, and the values it
    /// needs, present or not. Found by the astream-term differential oracle
    /// (aterm-conformance `tests/vt_oracle`): a truncated `38;5` turned on
    /// blink, because the refused colour returned its tail to the SGR loop.
    #[test]
    fn a_refused_extended_colour_consumes_what_xterm_consumes() {
        let mut term = Terminal::new(2, 16);
        for (seq, what) in [
            (&b"\x1b[31;44m\x1b[38;5m"[..], "38;5 with no index"),
            (b"\x1b[31;44m\x1b[48;5m", "48;5 with no index"),
            (b"\x1b[31;44m\x1b[58;5m", "58;5 with no index"),
            (b"\x1b[31;44m\x1b[38;2;1;2m", "38;2 one value short"),
            (b"\x1b[31;44m\x1b[48;2m", "48;2 with no values"),
        ] {
            term.process(b"\x1b[0m");
            term.process(seq);
            assert_eq!(
                term.style.flags,
                CellFlags::empty(),
                "{what}: no stray blink, faint or bold"
            );
            assert!(term.style.fg == PackedColor::indexed(1), "{what}: fg kept");
            assert!(term.style.bg == PackedColor::indexed(4), "{what}: bg kept");
            assert_eq!(term.transient.current_underline_color, None, "{what}");
        }
        term.process(b"\x1b[0m\x1b[38;7;1m");
        assert_eq!(term.style.flags, CellFlags::BOLD, "38;7;1");
        term.process(b"\x1b[0m\x1b[4;38m");
        assert_eq!(term.style.flags, CellFlags::UNDERLINE, "4;38");
        assert!(term.style.fg == PackedColor::DEFAULT_FG, "4;38");
    }
}
