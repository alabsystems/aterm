// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Character write path: translated characters, REP, and combining-mark / ZWJ continuation.

use std::sync::Arc;

use crate::grid::{Cell, CellFlags, row_u16};

use super::TerminalHandler;

/// Character width with tiered fast paths, agreeing with `aterm_grapheme`:
#[inline]
fn char_width(c: char, cjk: bool) -> usize {
    let cp = c as u32;
    if (0x20..0x7F).contains(&cp) || (!cjk && (0xA0..0x0300).contains(&cp)) {
        1
    } else if (0x4E00..0xA000).contains(&cp) {
        // Uniformly East Asian Wide in both modes (checked exhaustively by
        // `fast_path_width_matches_table`).
        2
    } else if (0x3000..0x4E00).contains(&cp) {
        // U+3000-U+4DFF (CJK symbols, kana, bopomofo, compatibility jamo, Yijing
        // hexagrams, enclosed CJK) mixes wide, narrow, ambiguous (U+3248-U+324F)
        // and zero-width marks (U+302A-302D, U+3099-309A): defer to the table so
        // the print path matches reflow/materialize/fill.
        if cjk {
            aterm_grapheme::char_width_cjk(c)
        } else {
            aterm_grapheme::char_width(c)
        }
    } else if (0xAC00..0xD7A4).contains(&cp) {
        // Hangul Syllables — always width 2
        2
    } else if (0x1F300..0x1F700).contains(&cp) {
        // SMP Emoji blocks (Misc Symbols, Emoticons, Ornamental Dingbats, Transport).
        smp_emoji_width(cp)
    } else if (0x2600..0x27C0).contains(&cp) {
        // BMP Misc Symbols + Dingbats (✨ U+2728, ⚡ U+26A1, zodiac, etc.).
        bmp_emoji_width(cp, cjk)
    } else if (0x20000..0x2FA20).contains(&cp) || (0x30000..0x323B0).contains(&cp) {
        // CJK Extensions B-H and the Compatibility Ideographs Supplement.
        2
    } else if cjk {
        aterm_grapheme::char_width_cjk(c)
    } else {
        aterm_grapheme::char_width(c)
    }
}

/// Bitmap of width-2 codepoints in U+1F300-U+1F6FF (128 bytes), from the
/// Unicode East Asian Width tables: bit set = width 2.
#[rustfmt::skip]
static SMP_EMOJI_WIDTH2: [u8; 128] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0x01, 0xE0, 0xBF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xDF,
    0xFF, 0xFF, 0x0F, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x87, 0x0F, 0x00, 0xFF, 0xFF, 0x11, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F, 0xFD, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x9F,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x3F, 0x00, 0x78, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x04,
    0x00, 0x00, 0x60, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xF8,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x3F, 0x10, 0xE7, 0xF0, 0x00, 0x18, 0xF0, 0x1F,
];

/// O(1) bitmap lookup for SMP emoji width (U+1F300-U+1F6FF).
#[inline]
fn smp_emoji_width(cp: u32) -> usize {
    let idx = (cp - 0x1F300) as usize;
    if SMP_EMOJI_WIDTH2[idx / 8] & (1 << (idx % 8)) != 0 {
        2
    } else {
        // SAFETY: `cp` is in `0x1F300..=0x1F6FF`, which are all valid Unicode scalar values.
        aterm_grapheme::char_width(unsafe { char::from_u32_unchecked(cp) }).max(1)
    }
}

/// Bitmap of width-2 codepoints in U+2600-U+27BF (56 bytes): Miscellaneous
/// Symbols and Dingbats.
#[rustfmt::skip]
static BMP_EMOJI_WIDTH2: [u8; 56] = [
    0x00, 0x00, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0x0F, 0x00, 0x00, 0x00, 0x00, 0x80,
    0x00, 0x00, 0x08, 0x00, 0x02, 0x0C, 0x00, 0x60, 0x30, 0x40, 0x10, 0x00, 0x00, 0x04, 0x2C, 0x24,
    0x20, 0x0C, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x50, 0xB8, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0xE0, 0x00, 0x00, 0x00, 0x01, 0x80,
];

/// O(1) bitmap lookup for BMP emoji width (U+2600-U+27BF).
#[inline]
fn bmp_emoji_width(cp: u32, cjk: bool) -> usize {
    let idx = (cp - 0x2600) as usize;
    if BMP_EMOJI_WIDTH2[idx / 8] & (1 << (idx % 8)) != 0 {
        2
    } else {
        // SAFETY: `cp` is in `0x2600..=0x27BF`, which are all valid Unicode scalar values.
        let c = unsafe { char::from_u32_unchecked(cp) };
        if cjk {
            aterm_grapheme::char_width_cjk(c).max(1)
        } else {
            aterm_grapheme::char_width(c).max(1)
        }
    }
}

impl TerminalHandler<'_> {
    /// Write a character to the grid with the current style.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "alt_screen",
            action = "WriteMain",
            project = "aterm_core::terminal::project_altscreen"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "alt_screen",
            action = "Scribble",
            project = "aterm_core::terminal::project_altscreen"
        )
    )]
    #[inline]
    pub(super) fn write_char(&mut self, c: char) {
        let origin = self.grid.output_damage_origin();
        self.write_char_body(c);
        self.grid.damage_selection_output(origin);
    }

    /// `write_char` without the output-damage bracket; split so every early
    /// return below stays inside the bracket.
    fn write_char_body(&mut self, c: char) {
        let translated = self.charset.translate(c);

        let width = char_width(translated, self.modes.ambiguous_width_double);

        if width == 0 {
            self.transient.last_combining_was_zwj = translated == '\u{200D}';
            self.add_combining_to_previous_cell(translated);
            if translated == '\u{FE0F}' {
                self.widen_previous_cell_for_vs16();
            }
            if translated == '\u{FE0E}' {
                self.narrow_previous_cell_for_vs15();
            }
            return;
        }

        self.transient.last_graphic_char = Some(c);

        if is_emoji_skin_tone_modifier(translated)
            && self.try_combine_skin_tone_modifier(translated)
        {
            return;
        }

        if is_regional_indicator(translated) && self.try_combine_regional_indicator(translated) {
            return;
        }

        if self.transient.last_combining_was_zwj
            && !translated.is_ascii()
            && self.should_combine_with_previous_zwj()
        {
            self.add_combining_to_previous_cell(translated);
            return;
        }
        self.transient.last_combining_was_zwj = false;

        self.write_char_core(translated, width);
    }

    /// Bulk write path for runs of 2+ consecutive multi-byte UTF-8 characters.
    #[allow(
        clippy::too_many_lines,
        reason = "hot-path character dispatch with many optimized branches"
    )]
    pub(super) fn write_unicode_bulk(&mut self, chars: &[char]) {
        // Preconditions: no insert mode, autowrap on, no style extras. VT52
        // addressing never reaches here (`print_unicode_bulk` routes it per
        // character). U+00A0-U+00FF may need GR-mapped translation when a
        // non-ASCII charset is designated on the GR G-set, so fall back then.
        if self.modes.insert_mode
            || !self.modes.auto_wrap
            || self.transient.has_transient_extras
            || self.style.has_style_extras()
            || self.transient.last_combining_was_zwj
            || !self.charset.gr_is_passthrough()
        {
            for &c in chars {
                self.write_char(c);
            }
            return;
        }
        self.charset.clear_single_shift();

        let cjk = self.modes.ambiguous_width_double;

        let margins_active = self.grid.has_horizontal_margins();

        let colors = self.style.cached_colors();
        let flags = if self.style.protected {
            self.style.flags.union(CellFlags::PROTECTED)
        } else {
            self.style.flags
        };
        let complex_flags = flags.union(CellFlags::COMPLEX);

        let mut last_graphic: Option<char> = None;

        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let cp = c as u32;

            if (0x3000..0xA000).contains(&cp) || (0xAC00..0xD7A4).contains(&cp) {
                // U+3000-U+4DFF is mixed-width: classify each char (honoring
                // `cjk`) and batch only width-2 cells.
                if (0x3000..0x4E00).contains(&cp) {
                    match char_width(c, cjk) {
                        0 => {
                            self.transient.last_combining_was_zwj = false;
                            self.add_combining_to_previous_cell(c);
                            i += 1;
                            continue;
                        }
                        1 => {
                            self.grid.write_narrow_autowrap_fast(c, colors, flags);
                            last_graphic = Some(c);
                            self.transient.last_combining_was_zwj = false;
                            i += 1;
                            continue;
                        }
                        _ => {}
                    }
                }
                let run_start = i;
                i += 1;
                while i < chars.len() {
                    let cp2 = chars[i] as u32;
                    let wide = (0x4E00..0xA000).contains(&cp2)
                        || (0xAC00..0xD7A4).contains(&cp2)
                        || ((0x3000..0x4E00).contains(&cp2) && char_width(chars[i], cjk) == 2);
                    if wide {
                        i += 1;
                    } else {
                        break;
                    }
                }
                if margins_active {
                    for &wc in &chars[run_start..i] {
                        self.grid.write_wide_autowrap_fast(wc, colors, flags);
                    }
                } else {
                    self.grid
                        .write_wide_run_autowrap(&chars[run_start..i], colors, flags);
                }
                last_graphic = Some(chars[i - 1]);
                self.transient.last_combining_was_zwj = false;
                continue;
            } else if cp > 0xFFFF {
                let width = char_width(c, cjk);
                if width == 0 {
                    self.transient.last_combining_was_zwj = c == '\u{200D}';
                    self.add_combining_to_previous_cell(c);
                    i += 1;
                    continue;
                }
                if width == 2 {
                    // A folded glyph is still REP's last graphic character, as in
                    // `write_char`.
                    if self.transient.last_combining_was_zwj
                        && self.should_combine_with_previous_zwj()
                    {
                        self.add_combining_to_previous_cell(c);
                        self.transient.last_combining_was_zwj = false;
                        last_graphic = Some(c);
                        i += 1;
                        continue;
                    }
                    if is_emoji_skin_tone_modifier(c) && self.try_combine_skin_tone_modifier(c) {
                        self.transient.last_combining_was_zwj = false;
                        last_graphic = Some(c);
                        i += 1;
                        continue;
                    }
                    if is_regional_indicator(c) && self.try_combine_regional_indicator(c) {
                        self.transient.last_combining_was_zwj = false;
                        last_graphic = Some(c);
                        i += 1;
                        continue;
                    }
                    self.transient.last_combining_was_zwj = false;
                    let run_start = i;
                    i += 1;
                    while i < chars.len() {
                        let c2 = chars[i];
                        let cp2 = c2 as u32;
                        if is_emoji_skin_tone_modifier(c2) || is_regional_indicator(c2) {
                            break;
                        }
                        if (0x20000..0x2FA20).contains(&cp2)
                            || (0x30000..0x323B0).contains(&cp2)
                            || char_width(c2, cjk) == 2
                        {
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    if margins_active {
                        for &wc in &chars[run_start..i] {
                            if (wc as u32) > 0xFFFF {
                                self.grid
                                    .write_emoji_autowrap_fast(wc, colors, complex_flags);
                            } else {
                                self.grid.write_wide_autowrap_fast(wc, colors, flags);
                            }
                        }
                    } else {
                        self.grid.write_mixed_wide_run_autowrap(
                            &chars[run_start..i],
                            colors,
                            flags,
                            complex_flags,
                        );
                    }
                    last_graphic = Some(chars[i - 1]);
                    continue;
                }
                self.write_char_core(c, width);
            } else if !cjk && (0xA0..0x300).contains(&cp) {
                // U+00A0-U+02FF: width 1 outside CJK mode.
                self.grid.write_narrow_autowrap_fast(c, colors, flags);
            } else {
                let width = char_width(c, cjk);
                if width == 0 {
                    self.transient.last_combining_was_zwj = c == '\u{200D}';
                    self.add_combining_to_previous_cell(c);
                    if c == '\u{FE0F}' {
                        self.widen_previous_cell_for_vs16();
                    }
                    if c == '\u{FE0E}' {
                        self.narrow_previous_cell_for_vs15();
                    }
                    i += 1;
                    if self.transient.last_combining_was_zwj {
                        if let Some(g) = last_graphic.take() {
                            self.transient.last_graphic_char = Some(g);
                        }
                        for &rest in &chars[i..] {
                            self.write_char(rest);
                        }
                        return;
                    }
                    continue;
                }
                if width == 2 {
                    self.grid.write_wide_autowrap_fast(c, colors, flags);
                } else {
                    self.grid.write_narrow_autowrap_fast(c, colors, flags);
                }
            }
            last_graphic = Some(c);
            self.transient.last_combining_was_zwj = false;
            i += 1;
        }

        if let Some(c) = last_graphic {
            self.transient.last_graphic_char = Some(c);
        }
    }

    /// Shared write path: insert, write at cursor, apply extras, advance. Extras
    /// (hyperlinks, non-BMP overflow, underline colors, RGB) are applied before
    /// the cursor advances, since an autowrap scroll would move the written row.
    fn write_char_core(&mut self, c: char, width: usize) {
        let mut flags = if self.style.protected {
            self.style.flags.union(CellFlags::PROTECTED)
        } else {
            self.style.flags
        };

        let is_non_bmp = (c as u32) > 0xFFFF;
        if is_non_bmp {
            flags = flags.union(CellFlags::COMPLEX);
        }

        let needs_style_extras =
            self.transient.has_transient_extras || self.style.has_style_extras();

        let colors = if needs_style_extras {
            self.style.cached_colors().with_extras_flag()
        } else {
            self.style.cached_colors()
        };

        if width == 2 && self.modes.auto_wrap && !self.modes.insert_mode && !needs_style_extras {
            if is_non_bmp {
                self.grid.write_emoji_autowrap_fast(c, colors, flags);
            } else {
                self.grid.write_wide_autowrap_fast(c, colors, flags);
            }
            return;
        }

        if self.modes.auto_wrap {
            self.grid.resolve_pending_wrap();
            if width == 1 {
                self.grid.wrap_if_past_print_edge();
            }
        } else {
            self.grid.set_pending_wrap(false);
        }

        let (did_write, ecols) = if width == 2 {
            let mut ecols = self.grid.effective_cols_for_current_row();
            if self.modes.auto_wrap {
                let fill = crate::grid::Cell::bce_blank(colors);
                ecols = self.grid.pre_wrap_wide_ecols(ecols, fill);
            }
            if self.modes.insert_mode {
                if !self.modes.auto_wrap && self.grid.cursor_col().saturating_add(1) >= ecols {
                    // Wide char doesn't fit — no-op, matching xterm.
                    return;
                }
                self.grid
                    .insert_chars_margin(row_u16(width), self.modes.left_right_margin_mode);
            }
            let ok = self
                .grid
                .write_wide_char_at_cursor_packed_ecols(c, colors, flags, ecols);
            (ok, ecols)
        } else {
            if self.modes.insert_mode {
                self.grid
                    .insert_chars_margin(row_u16(width), self.modes.left_right_margin_mode);
            }
            self.grid.write_char_at_cursor_packed(c, colors, flags);
            (true, 0)
        };

        if !did_write {
            return;
        }

        let row = self.grid.cursor_row();
        let col = self.grid.cursor_col();

        if is_non_bmp {
            self.grid.set_complex_char_ring(row, col, c);
        }
        if needs_style_extras {
            self.apply_cell_extras_preflagged(row, col, width);
        }

        if width == 2 {
            if self.modes.auto_wrap {
                self.grid.advance_cursor_wide_wrap_ecols(ecols);
            } else {
                self.grid.advance_cursor_wide_no_wrap_ecols(ecols);
            }
        } else if self.modes.auto_wrap {
            self.grid.advance_cursor_wrap();
        } else {
            self.grid.advance_cursor_no_wrap();
        }
    }

    /// Apply hyperlink, underline color, RGB, and extended flags to written cell(s).
    fn apply_cell_extras_preflagged(&mut self, row: u16, col: u16, width: usize) {
        let flags = if self.style.protected {
            self.style.flags.union(CellFlags::PROTECTED)
        } else {
            self.style.flags
        };

        let has_hyperlink = self.transient.current_hyperlink.is_some();
        let has_underline_color = self.transient.current_underline_color.is_some();
        let has_extended = flags.has_extended_flags();
        let fg_rgb = if self.style.fg.is_rgb() {
            Some(self.style.fg.rgb_components())
        } else {
            None
        };
        let bg_rgb = if self.style.bg.is_rgb() {
            Some(self.style.bg.rgb_components())
        } else {
            None
        };

        if !has_hyperlink
            && !has_underline_color
            && !has_extended
            && fg_rgb.is_none()
            && bg_rgb.is_none()
        {
            return;
        }

        let cols = if width == 2 && col + 1 < self.grid.cols() {
            2
        } else {
            1
        };
        for i in 0..cols {
            let extra = self.grid.cell_extra_mut_preflagged(row, col + i);
            if let Some(ref hyperlink) = self.transient.current_hyperlink {
                extra.set_hyperlink(Some(Arc::clone(hyperlink)));
                if let Some(ref id) = self.transient.current_hyperlink_id {
                    extra.set_hyperlink_id(Some(Arc::clone(id)));
                }
            }
            if let Some(color) = self.transient.current_underline_color {
                extra.set_underline_color_u32(Some(color));
            }
            if has_extended {
                extra.set_extended_flags(flags.extended_flags().bits());
            }
            if let Some((r, g, b)) = fg_rgb {
                extra.set_fg_rgb(Some([r, g, b]));
            }
            if let Some((r, g, b)) = bg_rgb {
                extra.set_bg_rgb(Some([r, g, b]));
            }
        }

        if has_hyperlink {
            self.grid.enforce_hyperlink_limit();
        }
    }

    /// The cell a zero-width mark attaches to: [`Self::preceding_cell`], or with
    /// none (column 0 of a row that is not a soft-wrap continuation) the cell
    /// under the cursor, as in xterm (`use_col = char_was_written ?
    /// last_written_col : cur_col`).
    #[inline]
    fn combining_target_cell(&self) -> (u16, u16) {
        self.preceding_cell()
            .unwrap_or((self.grid.cursor_row(), self.grid.cursor_col()))
    }

    /// Find the previous effective cell, skipping wide continuation cells.
    fn preceding_cell(&self) -> Option<(u16, u16)> {
        let row = self.grid.cursor_row();
        let col = self.grid.cursor_col();

        if col == 0 && row == 0 && !self.grid.pending_wrap() {
            return None;
        }

        let (target_row, target_col) = if self.grid.pending_wrap() {
            (row, col)
        } else if col > 0 {
            (row, col - 1)
        } else {
            let is_continuation = self.grid.row(row).is_some_and(aterm_grid::Row::is_wrapped);
            if !is_continuation {
                return None;
            }
            let prev_row = row.saturating_sub(1);
            (
                prev_row,
                self.grid.effective_cols_for_row(prev_row).saturating_sub(1),
            )
        };

        let (final_row, final_col) =
            if target_col > 0 && self.grid.is_wide_continuation_at(target_row, target_col) {
                (target_row, target_col - 1)
            } else {
                (target_row, target_col)
            };

        Some((final_row, final_col))
    }

    /// Add a combining character to the cell it belongs to: the base in the
    /// previous cell (the main cell of a wide character), or the cell under the
    /// cursor when there is none (see [`Self::combining_target_cell`]).
    fn add_combining_to_previous_cell(&mut self, combining: char) {
        let (row, col) = self.combining_target_cell();
        self.grid.cell_extra_mut(row, col).add_combining(combining);
        if let Some(r) = self.grid.row_mut(row) {
            r.update_len(col.saturating_add(1));
        }
        self.grid.mark_content_cell(row, col);
    }

    /// Whether the previous cell ends with ZWJ, so the next emoji joins it into
    /// one grapheme (e.g. 👨 + ZWJ + 💻).
    fn should_combine_with_previous_zwj(&self) -> bool {
        const ZWJ: char = '\u{200D}';

        let Some((row, col)) = self.preceding_cell() else {
            return false;
        };

        self.grid.cell_extra(row, col).is_some_and(|extra| {
            extra
                .combining()
                .last()
                .copied()
                .or_else(|| extra.complex_char().and_then(|s| s.chars().next_back()))
                == Some(ZWJ)
        })
    }

    /// Widen the previous cell from 1 to 2 cells when VS16 (U+FE0F) follows an
    /// emoji-capable base (as kitty, WezTerm and foot do): set WIDE on the base,
    /// write a WIDE_CONTINUATION spacer in the next column, and advance the
    /// cursor.
    fn widen_previous_cell_for_vs16(&mut self) {
        let Some((row, col)) = self.preceding_cell() else {
            return;
        };

        let Some(cell) = self.grid.cell(row, col) else {
            return;
        };
        if cell.is_wide() {
            return;
        }

        let base_char = if cell.is_complex() {
            self.grid.resolved_char(row, col).unwrap_or('\u{FFFD}')
        } else {
            cell.char()
        };
        if !is_vs16_emoji_capable(base_char) {
            return;
        }

        let char_data = cell.char_data();
        let colors = cell.colors();
        let base_flags = cell.flags();

        let cont_col = col + 1;

        if cont_col >= self.grid.effective_cols_for_row(row) {
            return;
        }

        let wide_base =
            crate::grid::Cell::from_raw_parts(char_data, colors, base_flags.union(CellFlags::WIDE));
        let cont_cell = crate::grid::Cell::from_raw_parts(
            ' ' as u16,
            colors.without_extras_flag(),
            base_flags.wide_continuation_of(),
        );
        let ecols = self.grid.effective_cols_for_row(row);
        let orphan_col = if self.grid.cell(row, cont_col).is_some_and(Cell::is_wide) {
            let oc = cont_col + 1;
            if oc < ecols { Some(oc) } else { None }
        } else {
            None
        };

        if let Some(row_data) = self.grid.row_mut(row) {
            // Wide char fixup: if the cell we're about to overwrite with
            // WIDE_CONTINUATION was itself the first half of a wide char,
            // clear the orphaned continuation at cont_col + 1.
            if row_data
                .flags()
                .contains(crate::grid::RowFlags::HAS_WIDE_CHARS)
            {
                if let Some(existing) = row_data.get(cont_col) {
                    if existing.flags().contains(CellFlags::WIDE) {
                        let orphan_col = cont_col + 1;
                        if orphan_col < row_data.cols() {
                            row_data.set(orphan_col, crate::grid::Cell::EMPTY);
                        }
                    }
                }
            }
            row_data.set(col, wide_base);
            row_data.set(cont_col, cont_cell);
        }
        self.grid
            .drop_unclaimed_extras(row, cont_col..cont_col.saturating_add(2));
        self.grid.clear_rgb_ring_cells(row, cont_col, 1);

        self.mirror_vs16_spacer_extras(row, col, cont_col);

        if let Some(oc) = orphan_col {
            self.grid.damage_mut().mark_cell(row, oc);
        }
        self.grid.damage_mut().mark_cell(row, col);
        self.grid.damage_mut().mark_cell(row, cont_col);

        if self.grid.pending_wrap() {
            // Base was written at the last column; keep pending_wrap.
        } else if self.modes.auto_wrap {
            self.grid.advance_cursor_wrap();
        } else {
            self.grid.advance_cursor_no_wrap();
        }
    }

    /// Copy the base cell's rendition-valued extras (truecolor fg/bg and the
    /// SGR 58 underline colour) onto the spacer a VS16 widening created.
    fn mirror_vs16_spacer_extras(&mut self, row: u16, lead: u16, spacer: u16) {
        let lead_cell = self.grid.cell(row, lead).copied();
        let fg = lead_cell
            .is_some_and(|c| c.fg_needs_overflow())
            .then(|| self.grid.fg_rgb_at(row, lead))
            .flatten();
        let bg = lead_cell
            .is_some_and(|c| c.bg_needs_overflow())
            .then(|| self.grid.bg_rgb_at(row, lead))
            .flatten();
        let underline = self
            .grid
            .cell_extra(row, lead)
            .and_then(crate::grid::CellExtra::underline_color_u32);
        if fg.is_none() && bg.is_none() && underline.is_none() {
            // Nothing to carry — and no empty entry (or HAS_EXTRAS bit) created
            // for the overwhelmingly common plain emoji.
            return;
        }
        let extra = self.grid.cell_extra_mut(row, spacer);
        if fg.is_some() {
            extra.set_fg_rgb(fg);
        }
        if bg.is_some() {
            extra.set_bg_rgb(bg);
        }
        if underline.is_some() {
            extra.set_underline_color_u32(underline);
        }
    }

    /// Narrow the previous cell from 2 to 1 cell when VS15 (U+FE0E) follows a
    /// wide emoji: clear WIDE on the base and empty the spacer.
    fn narrow_previous_cell_for_vs15(&mut self) {
        let Some((row, col)) = self.preceding_cell() else {
            return;
        };

        let Some(cell) = self.grid.cell(row, col) else {
            return;
        };
        if !cell.is_wide() {
            return;
        }

        let char_data = cell.char_data();
        let colors = cell.colors();
        let base_flags = cell.flags();

        let cont_col = col + 1;
        if cont_col >= self.grid.effective_cols_for_row(row) {
            return;
        }

        let narrow_base = crate::grid::Cell::from_raw_parts(
            char_data,
            colors,
            base_flags.difference(CellFlags::WIDE),
        );

        if let Some(row_data) = self.grid.row_mut(row) {
            row_data.set(col, narrow_base);
            row_data.set(cont_col, crate::grid::Cell::EMPTY);
        }
        self.grid
            .drop_unclaimed_extras(row, cont_col..cont_col.saturating_add(1));

        self.grid.damage_mut().mark_cell(row, col);
        self.grid.damage_mut().mark_cell(row, cont_col);
    }

    /// Combine an emoji skin tone modifier (U+1F3FB-U+1F3FF) into the previous
    /// emoji cell.
    fn try_combine_skin_tone_modifier(&mut self, modifier: char) -> bool {
        let Some((row, col)) = self.preceding_cell() else {
            return false;
        };

        let Some(cell) = self.grid.cell(row, col) else {
            return false;
        };

        if !cell.is_wide() {
            return false;
        }

        let base_char = if cell.is_complex() {
            self.grid.resolved_char(row, col).unwrap_or('\u{FFFD}')
        } else {
            cell.char()
        };

        if !aterm_grapheme::is_emoji_modifier_base(base_char) {
            return false;
        }

        if aterm_grapheme::char_width(base_char) < 2
            && !self.grid.cell_extra(row, col).is_some_and(|e| {
                e.combining().contains(&'\u{FE0F}')
                    || e.complex_char().is_some_and(|s| s.contains('\u{FE0F}'))
            })
        {
            return false;
        }

        self.add_combining_to_previous_cell(modifier);
        true
    }

    /// Combine the second regional indicator of a flag pair into the first RI's
    /// cell, so `🇺🇸` is one 2-cell grapheme.
    fn try_combine_regional_indicator(&mut self, ri: char) -> bool {
        let Some((row, col)) = self.preceding_cell() else {
            return false;
        };
        let Some(cell) = self.grid.cell(row, col) else {
            return false;
        };
        let base = if cell.is_complex() {
            self.grid.resolved_char(row, col).unwrap_or('\u{FFFD}')
        } else {
            cell.char()
        };
        if !is_regional_indicator(base) {
            return false;
        }
        let already_paired = self.grid.cell_extra(row, col).is_some_and(|e| {
            let in_base = e.complex_char().map_or(1, |s| {
                s.chars().filter(|&c| is_regional_indicator(c)).count()
            });
            let in_marks = e
                .combining()
                .iter()
                .filter(|&&c| is_regional_indicator(c))
                .count();
            in_base + in_marks >= 2
        });
        if already_paired {
            return false;
        }
        self.add_combining_to_previous_cell(ri);
        true
    }
}

/// Whether a character is a regional indicator (U+1F1E6-U+1F1FF). A local
/// copy of aterm-grapheme's, which its private `width` module keeps for its grid.
#[inline]
const fn is_regional_indicator(c: char) -> bool {
    matches!(c as u32, 0x1F1E6..=0x1F1FF)
}

/// Whether a character is an emoji skin tone modifier (U+1F3FB-U+1F3FF, UTS #51).
#[inline]
const fn is_emoji_skin_tone_modifier(c: char) -> bool {
    matches!(c as u32, 0x1F3FB..=0x1F3FF)
}

/// Whether `c` is eligible for VS16 emoji presentation widening ([`aterm_grapheme::is_vs16_emoji_capable`]).
#[inline]
pub(crate) fn is_vs16_emoji_capable(c: char) -> bool {
    aterm_grapheme::is_vs16_emoji_capable(c)
}

#[cfg(test)]
mod tests {
    use super::char_width;
    use crate::terminal::Terminal;

    const ENGLAND: &str = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}";

    /// `text` written into a fresh 1×8 terminal in one `process` call (the bulk
    /// arms) and one scalar per call (the per-char writer); the two grids must
    /// agree.
    fn whole_and_fragmented(text: &str) -> [Terminal; 2] {
        let mut whole = Terminal::new(1, 8);
        whole.process(text.as_bytes());
        let mut fragmented = Terminal::new(1, 8);
        let mut buf = [0u8; 4];
        for c in text.chars() {
            fragmented.process(c.encode_utf8(&mut buf).as_bytes());
        }
        [whole, fragmented]
    }

    /// A tag character (U+E0000..U+E007F) or Variation Selectors Supplement
    /// scalar (U+E0100..U+E01EF) is zero-width: it attaches to the preceding
    /// cell and the cursor does not advance.
    #[test]
    fn tag_and_vs_supplement_attach_to_the_previous_cell() {
        for (text, mark, x_col) in [
            ("A\u{E0001}X", '\u{E0001}', 1u16),
            ("A\u{E007F}X", '\u{E007F}', 1),
            ("e\u{E0100}X", '\u{E0100}', 1),
            ("e\u{E01EF}X", '\u{E01EF}', 1),
            ("\u{00E9}\u{E0001}X", '\u{E0001}', 1),
            ("\u{4E00}\u{E0100}X", '\u{E0100}', 2),
        ] {
            for term in whole_and_fragmented(text) {
                let grid = term.grid();
                assert_eq!(
                    grid.cell(0, x_col).map(|c| c.char()),
                    Some('X'),
                    "{text:?}: X is adjacent to the base"
                );
                assert_eq!(term.cursor().col, x_col + 1, "{text:?}: cursor after X");
                assert_eq!(
                    grid.cell_extra(0, 0).map(|e| e.combining()),
                    Some([mark].as_slice()),
                    "{text:?}: the mark attached to the base cell"
                );
                assert_eq!(
                    grid.extras().len(),
                    1,
                    "{text:?}: only the base cell carries an extra"
                );
            }
        }
    }

    /// A combining mark with no cell to its left lands on the cell under the
    /// cursor and never vanishes: `printf 'abc\r'` then U+0301 accents the 'a',
    /// as in xterm.
    #[test]
    fn a_combining_mark_with_no_cell_to_its_left_lands_on_the_cursor_cell() {
        for (input, col, base) in [
            ("abc\r\u{301}", 0u16, 'a'),
            ("abc\u{1b}[1;1H\u{301}", 0, 'a'),
            ("\u{301}", 0, ' '),
        ] {
            let mut term = Terminal::new(1, 8);
            term.process(input.as_bytes());
            assert_eq!(
                term.grid().cell(0, col).map(|c| c.char()),
                Some(base),
                "{input:?}: the base cell is untouched"
            );
            assert_eq!(
                term.grid().cell_extra(0, col).map(|e| e.combining()),
                Some(['\u{301}'].as_slice()),
                "{input:?}: the mark attached to the cell under the cursor \
                 instead of being discarded"
            );
            assert_eq!(
                term.cursor().col,
                col,
                "{input:?}: a zero-width mark never moves the cursor"
            );
        }
    }

    /// After a cursor motion that leaves a cell to the left, the mark goes to
    /// that cell (the positional rule).
    #[test]
    fn a_combining_mark_after_a_cursor_motion_uses_the_cell_to_the_left() {
        let mut term = Terminal::new(1, 8);
        term.process("abc\u{1b}[2D\u{301}".as_bytes());
        assert_eq!(term.cursor().col, 1, "the cursor sits over the 'b'");
        assert_eq!(
            term.grid().cell_extra(0, 0).map(|e| e.combining()),
            Some(['\u{301}'].as_slice()),
            "the mark accents the 'a', the cell to the LEFT (iTerm2/Ghostty/kitty); \
             xterm would accent the 'b' here"
        );
        assert!(
            term.grid().cell_extra(0, 1).is_none(),
            "the cell under the cursor is untouched while a left cell exists"
        );
    }

    /// England: 🏴 + gbeng + CANCEL TAG is one two-cell grapheme whose six
    /// tags all attach to the lead cell (well under `MAX_COMBINING`).
    #[test]
    fn subdivision_flag_occupies_two_cells() {
        for term in whole_and_fragmented(&format!("{ENGLAND}X")) {
            let grid = term.grid();
            assert!(
                grid.cell(0, 0).is_some_and(|c| c.is_wide()),
                "the flag's lead cell is wide"
            );
            assert_eq!(grid.cell(0, 2).map(|c| c.char()), Some('X'));
            assert_eq!(term.cursor().col, 3);
            assert_eq!(
                grid.cell_extra(0, 0).map(|e| e.combining().len()),
                Some(6),
                "all six tags attached to the lead cell"
            );
        }
    }

    /// VS16 widens exactly the bases Unicode gives an emoji presentation
    /// sequence (`Emoji=Yes`, `Emoji_Presentation=No`): `⬆️` is two cells, as
    /// `wcwidth`, `string-width` and every other terminal count it, so the
    /// program that wrote it puts its next character where the grid does; `✓️`
    /// has no such sequence and stays one cell.
    #[test]
    fn vs16_widens_exactly_the_text_default_emoji() {
        for (text, wide, x_col) in [
            ("\u{2764}\u{FE0F}X", true, 2u16),
            ("\u{2B06}\u{FE0F}X", true, 2),
            ("\u{2B05}\u{FE0F}X", true, 2),
            ("\u{2713}\u{FE0F}X", false, 1),
            ("\u{2605}\u{FE0F}X", false, 1),
            ("\u{1FB00}\u{FE0F}X", false, 1),
        ] {
            for term in whole_and_fragmented(text) {
                let grid = term.grid();
                assert_eq!(
                    grid.cell(0, 0).is_some_and(|c| c.is_wide()),
                    wide,
                    "{text:?}"
                );
                assert_eq!(grid.cell(0, x_col).map(|c| c.char()), Some('X'), "{text:?}");
                assert_eq!(term.cursor().col, x_col + 1, "{text:?}");
            }
        }
    }

    /// A syllable spelled in conjoining jamo is one two-cell cluster (UAX #29
    /// GB6-GB8) whichever block its vowel and final come from: Hangul Jamo
    /// Extended-B's V and T are zero-width like U+1160..U+11FF, as glibc and
    /// unicode-width have them.
    #[test]
    fn extended_b_vowels_and_finals_join_the_syllable() {
        for text in [
            "\u{1100}\u{1161}\u{11A8}|",
            "\u{A960}\u{D7B0}\u{D7CB}|",
            "\u{1100}\u{D7C6}\u{D7FB}|",
        ] {
            for term in whole_and_fragmented(text) {
                let grid = term.grid();
                assert!(grid.cell(0, 0).is_some_and(|c| c.is_wide()), "{text:?}");
                assert_eq!(grid.cell(0, 2).map(|c| c.char()), Some('|'), "{text:?}");
                assert_eq!(term.cursor().col, 3, "{text:?}");
            }
        }
    }

    /// Exhaustive parity guard for the width fast paths: `char_width` must
    /// agree with the `aterm_grapheme` table for every codepoint of each block
    /// it special-cases, in both ambiguous-width modes, since
    /// reflow/materialize/fill recompute width from the table.
    #[test]
    fn fast_path_width_matches_table() {
        let blocks: &[(&str, std::ops::Range<u32>)] = &[
            ("Misc Symbols and Dingbats", 0x2600..0x27C0),
            ("CJK", 0x3000..0xA000),
            ("Hangul Jamo", 0x1100..0x1200),
            ("Hangul Jamo Extended-A", 0xA960..0xA980),
            ("Hangul Syllables", 0xAC00..0xD7B0),
            ("Hangul Jamo Extended-B", 0xD7B0..0xD800),
        ];
        for (block, range) in blocks {
            for cp in range.clone() {
                let c = char::from_u32(cp).expect("every block is BMP scalar values");
                assert_eq!(
                    char_width(c, false),
                    aterm_grapheme::char_width(c),
                    "{block}: non-CJK (ambiguous=narrow) width mismatch at U+{cp:04X}"
                );
                assert_eq!(
                    char_width(c, true),
                    aterm_grapheme::char_width_cjk(c),
                    "{block}: CJK (ambiguous=wide) width mismatch at U+{cp:04X}"
                );
            }
        }
        assert_eq!((char_width('★', false), char_width('★', true)), (1, 2));
        assert_eq!((char_width('✨', false), char_width('✨', true)), (2, 2));
        for c in ['\u{AC00}', '\u{D55C}', '\u{AD6D}', '\u{C5B4}', '\u{D7A3}'] {
            assert_eq!(
                char_width(c, false),
                2,
                "U+{:04X} must occupy two cells (ambiguous=narrow)",
                c as u32
            );
            assert_eq!(
                char_width(c, true),
                2,
                "U+{:04X} must occupy two cells (ambiguous=wide)",
                c as u32
            );
        }
    }

    /// Absolute widths at the fast path's edge cases, independent of the table.
    #[test]
    fn known_cjk_width_regressions() {
        assert_eq!(char_width('\u{4DC0}', false), 2);
        assert_eq!(char_width('\u{4DC0}', true), 2);
        assert_eq!(char_width('\u{4DFF}', false), 2);

        assert_eq!(char_width('\u{303F}', false), 1);
        assert_eq!(char_width('\u{303F}', true), 1);

        assert_eq!(char_width('\u{3248}', false), 1);
        assert_eq!(char_width('\u{3248}', true), 2);
        assert_eq!(char_width('\u{324F}', false), 1);
        assert_eq!(char_width('\u{324F}', true), 2);

        assert_eq!(char_width('\u{302E}', false), 2);
        assert_eq!(char_width('\u{302F}', false), 2);

        assert_eq!(char_width('\u{302A}', false), 0);
        assert_eq!(char_width('\u{302D}', false), 0);
        assert_eq!(char_width('\u{3099}', false), 0);
        assert_eq!(char_width('\u{309A}', false), 0);

        assert_eq!(char_width('\u{3040}', false), 1);
        assert_eq!(char_width('\u{4E00}', false), 2);
        assert_eq!(char_width('\u{9FFF}', false), 2);
    }

    /// End to end with `ambiguous_width = "wide"`: an EAW=A symbol from U+2600..
    #[test]
    fn ambiguous_misc_symbols_take_two_cells_in_wide_mode() {
        use crate::config::TerminalConfig;
        let config = TerminalConfig {
            ambiguous_width_double: true,
            ..TerminalConfig::default()
        };
        for (text, col) in [("★|", 3u16), ("°|", 3), ("♪♥❶|", 7), ("中★中★|", 9)] {
            let mut term = Terminal::new(1, 20);
            term.apply_config(&config);
            term.process(text.as_bytes());
            assert_eq!(term.cursor().col, col, "{text:?}");
        }
        let mut term = Terminal::new(1, 20);
        term.process("★|".as_bytes());
        assert_eq!(term.cursor().col, 2);
    }

    /// A skin tone folds only onto a base wide by emoji presentation, never onto
    /// one `ambiguous_width = "wide"` widened: ⛹🏽 (U+26F9 is East Asian
    /// Ambiguous and an emoji modifier base) is 2 + 2 cells in wide mode, as 1 + 2
    /// in narrow, and keeps that layout through a checkpoint carry and history.
    #[test]
    fn skin_tone_folds_onto_an_emoji_presentation_base_in_both_width_modes() {
        use crate::config::TerminalConfig;
        let bar_col = |term: &Terminal| {
            (0..20).find(|&c| term.grid().cell(0, c).is_some_and(|x| x.char() == '|'))
        };
        for wide in [false, true] {
            let config = TerminalConfig {
                ambiguous_width_double: wide,
                ..TerminalConfig::default()
            };
            for (text, bar) in [
                ("\u{26F9}\u{1F3FD}|", if wide { 4u16 } else { 3 }),
                (
                    "a\u{26F9}\u{1F3FD}\u{26F9}\u{1F3FD}|",
                    if wide { 9 } else { 7 },
                ),
                ("\u{26F9}\u{FE0F}\u{1F3FD}|", 2),
                ("\u{1F44D}\u{1F3FD}|", 2),
            ] {
                let mut live = Terminal::new(1, 20);
                live.apply_config(&config);
                live.process(text.as_bytes());
                assert_eq!(bar_col(&live), Some(bar), "live {text:?} wide={wide}");
                assert_eq!(live.cursor().col, bar + 1, "live {text:?} wide={wide}");

                let cp = live.checkpoint_carry(0).expect("Ground");
                let mut adopted = Terminal::new(1, 20);
                adopted.apply_config(&config);
                adopted.restore_checkpoint(&cp);
                let fresh = Terminal::from_checkpoint(&cp);
                for (at, restored) in [("restore", &adopted), ("from_checkpoint", &fresh)] {
                    assert_eq!(bar_col(restored), Some(bar), "{at} {text:?} wide={wide}");
                }

                let mut scrolled = Terminal::new(1, 20);
                scrolled.apply_config(&config);
                scrolled.process(text.as_bytes());
                scrolled.process(b"\r\n");
                scrolled.resize(4, 19);
                let back = (0..4).find_map(|r| {
                    (0..19).find(|&c| scrolled.grid().cell(r, c).is_some_and(|x| x.char() == '|'))
                });
                assert_eq!(back, Some(bar), "history {text:?} wide={wide}");
            }
            let mut live = Terminal::new(1, 20);
            live.apply_config(&config);
            live.process("\u{1F590}\u{FE0F}".as_bytes());
            let cp = live.checkpoint_carry(0).expect("Ground");
            let mut adopted = Terminal::new(1, 20);
            adopted.apply_config(&config);
            adopted.restore_checkpoint(&cp);
            let mut fresh = Terminal::from_checkpoint(&cp);
            fresh.apply_config(&config);
            for (at, restored) in [("restore", &mut adopted), ("from_checkpoint", &mut fresh)] {
                restored.process("\u{1F3FD}|".as_bytes());
                assert_eq!(bar_col(restored), Some(2), "{at} split carry wide={wide}");
            }
        }
    }

    /// End-to-end pin for the wide-glyph wrap tail, driven as bytes so the SGR
    /// state, the BCE cursor template and the glyph colours share one source
    /// (the coupling `Grid::blank_wide_wrap_tail` relies on).
    #[test]
    fn a_wide_glyph_wrapping_off_the_last_column_repaints_the_orphan_it_bisects() {
        let mut term = Terminal::new(3, 10);
        term.process(b"\x1b[48;2;10;20;30m\x1b[1;9H\xe4\xb8\xad");
        assert_eq!(
            term.grid().bg_rgb_at(0, 8),
            Some([10, 20, 30]),
            "precondition: the first glyph sits at cols 8-9 on its own bg"
        );

        term.process(b"\x1b[48;2;200;100;50m\x1b[1;10H\xe4\xb8\xad");

        let grid = term.grid();
        assert_eq!(
            grid.cell(1, 0).map(|c| c.char()),
            Some('\u{4E2D}'),
            "precondition: the second glyph wrapped to the next row"
        );
        let tail = grid.cell(0, 9).expect("the blanked tail cell exists");
        let orphan = grid.cell(0, 8).expect("the orphaned head exists");
        assert_eq!(orphan.char(), ' ', "the orphaned wide head is blanked");
        assert!(
            !orphan.is_empty(),
            "a default-coloured orphan renders as a hole beside the blanked tail"
        );
        assert_eq!(
            orphan.colors(),
            tail.colors(),
            "the orphan must carry the same BCE blank as the tail that bisected it"
        );
        assert_eq!(
            grid.bg_rgb_at(0, 8),
            Some([200, 100, 50]),
            "the orphan must resolve the NEW background; without the ring \
             extension it resolves the dead glyph's [10, 20, 30]"
        );
        assert_eq!(
            grid.bg_rgb_at(0, 9),
            Some([200, 100, 50]),
            "the blanked tail must resolve it too — this site kept no ring at \
             all, so even the cell inside the blanked span was stale"
        );
    }
}
