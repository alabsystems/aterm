// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! SGR (Select Graphic Rendition) handler.

use crate::grid::{CellFlags, PackedColor};

use super::handler::SgrStyleHandler;
use super::sgr_color_u8;

/// What a colour's colon group names.
#[derive(Clone, Copy)]
enum ColonColour {
    Rgb(u8, u8, u8),
    Indexed(u8),
}

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

    /// Extra parameters consumed by an extended color code (`38`, `48`, `58`) in all semicolons.
    ///
    /// CMY and CMYK render nowhere, so they consume their values and change nothing. xterm
    /// consumes only the selector and runs the values as SGR codes; T.416 has no semicolon form
    /// of them to say otherwise, and a colour's values are not attributes.
    #[inline]
    const fn extended_color_skip(params: &[u16]) -> usize {
        if params.len() < 2 {
            return 0; // a bare 38 at the end of the list
        }
        match params[1] {
            2 | 3 => 4, // 38;2;r;g;b, 38;3;c;m;y
            4 => 5,     // 38;4;c;m;y;k
            5 => 2,     // 38;5;n
            _ => 1,     // an unknown selector consumes itself
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
    ///
    /// **One rule for 38, 48 and 58, `astream_term`'s `Screen::sgr` too.** A colon on the code or
    /// on the selector makes a group that is all the colour's (`38:2:…`, `38;2:…`: "after the
    /// first colon, colons must be used", xterm's ctlseqs), and the group is read by
    /// [`Self::colon_colour`]. Otherwise it is all semicolons, read by
    /// [`Self::parse_extended_color`]. A sub-parameter is never a code: a group on a code that
    /// takes none (`1:4`) is ignored with its code, as in xterm and the fold, and one a semicolon
    /// colour's values stopped short of (`38;5;9:4`) is skipped, as in the fold, where xterm runs
    /// it as a code. An empty last element is an element (`38:2:cs:r:g:` has a blue of 0, as in
    /// xterm and the fold), which the parser delivers as it delivers a `;`'s.
    ///
    /// **The rule's limit is the parser's cap of 24 parameters.** Past it values are dropped, so a
    /// group can arrive shorter than it was written, and the bit past the last parameter says a
    /// sub-parameter fell. A colour left with four values then reads as nothing: they are not the
    /// three channels, but the id form with its blue gone. The fold has no such cap and draws what
    /// was written, which the oracle excludes (`sgr_past_the_parameter_cap`).
    #[inline]
    pub(super) fn handle_sgr_with_subparams(&mut self, params: &[u16], subparam_mask: u32) {
        if params.is_empty() {
            self.style.reset_sgr();
            self.transient.current_underline_color = None;
            self.transient.update_has_transient_extras();
            self.apply_style_change();
            return;
        }

        let is_sub = |k: usize| k < 32 && (subparam_mask & (1u32 << k)) != 0;
        let group_end = |start: usize| {
            let mut end = start + 1;
            while end < params.len() && is_sub(end) {
                end += 1;
            }
            end
        };

        let mut i = 0;
        while i < params.len() {
            let end = group_end(i);
            if is_sub(i) {
                i = end;
                continue;
            }
            let param = params[i];

            if matches!(param, 38 | 48 | 58) {
                let group = if end > i + 1 {
                    Some((i + 1, end))
                } else {
                    let selector_end = group_end(end);
                    (end < params.len() && selector_end > end + 1).then_some((end, selector_end))
                };
                if let Some((start, stop)) = group {
                    let cut = stop == params.len() && is_sub(stop);
                    if let Some(colour) = Self::colon_colour(&params[start..stop], cut) {
                        self.set_extended_colour(param, colour);
                    }
                    i = stop;
                    continue;
                }
            } else if end > i + 1 {
                if param == 4 {
                    self.apply_underline_style(params[i + 1]);
                }
                i = end;
                continue;
            }

            i += self.apply_sgr_param(params, i);
            i += 1;
        }

        self.apply_style_change();
    }

    /// `4:n`: the underline style `n` names, `0` for none.
    #[inline]
    fn apply_underline_style(&mut self, style: u16) {
        self.style.flags.remove(CellFlags::ALL_UNDERLINES);
        match style {
            0 => {}
            2 => self.style.flags.insert(CellFlags::DOUBLE_UNDERLINE),
            3 => self.style.flags.insert(CellFlags::CURLY_UNDERLINE),
            4 => self.style.flags.insert(CellFlags::DOTTED_UNDERLINE),
            5 => self.style.flags.insert(CellFlags::DASHED_UNDERLINE),
            _ => self.style.flags.insert(CellFlags::UNDERLINE),
        }
    }

    /// Set the colour `code` (38, 48 or 58) selects.
    #[inline]
    fn set_extended_colour(&mut self, code: u16, colour: ColonColour) {
        let packed = match colour {
            ColonColour::Rgb(r, g, b) => PackedColor::rgb(r, g, b),
            ColonColour::Indexed(n) => PackedColor::indexed(n),
        };
        match code {
            38 => self.style.fg = packed,
            48 => self.style.bg = packed,
            _ => {
                self.transient.current_underline_color = Some(match colour {
                    ColonColour::Rgb(r, g, b) => {
                        0x01_000000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
                    }
                    ColonColour::Indexed(n) => 0x02_000000 | u32::from(n),
                });
                self.transient.update_has_transient_extras();
            }
        }
    }

    /// A colour's colon group, its selector first, wherever the first colon fell (`38:2:…`,
    /// `38;2:…`). `5:n` selects a palette colour. `2:cs:r:g:b` selects a 24-bit colour, the second
    /// element the colour-space id (ITU T.416 13.1.8, empty in `2::r:g:b`) and any past the blue
    /// its elements 6 to 8, none of which means anything here. `2:r:g:b`, which programs emit, has
    /// no id slot: only the count tells them apart, four values or three, as in xterm's
    /// `parse_extended_colors`, Ghostty, kitty and iTerm2, and a `cut` group (the parser's cap
    /// dropped what followed it) has lost the count. A shorter group, and any other selector (CMY
    /// `3:cs:c:m:y`, CMYK `4:cs:c:m:y:k`), names no colour.
    #[inline]
    const fn colon_colour(group: &[u16], cut: bool) -> Option<ColonColour> {
        match *group {
            [5, n, ..] => Some(ColonColour::Indexed(sgr_color_u8(n))),
            [2, _, r, g, b, ..] => Some(ColonColour::Rgb(
                sgr_color_u8(r),
                sgr_color_u8(g),
                sgr_color_u8(b),
            )),
            [2, r, g, b] if !cut => Some(ColonColour::Rgb(
                sgr_color_u8(r),
                sgr_color_u8(g),
                sgr_color_u8(b),
            )),
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
}

#[cfg(test)]
#[path = "handler_sgr_tests.rs"]
mod tests;
