// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Character write path: translated characters, REP, and combining-mark / ZWJ continuation.

use std::sync::Arc;

use crate::grid::{Cell, CellFlags, row_u16};

use super::TerminalHandler;

/// Character width with tiered fast paths that agree with `aterm_grapheme`: printable ASCII,
/// and U+00A0-U+02FF outside CJK mode (it holds ambiguous characters), are width 1; the
/// uniformly wide CJK ideograph and Hangul blocks are width 2; the emoji blocks read a
/// bitmap before the table; and everything else asks `aterm_grapheme::char_width`
/// (`char_width_cjk` when `cjk`). Inlined into every caller: the bulk writer classifies each
/// character of a run by it, and a call per character cost more than the lookup.
#[allow(
    clippy::inline_always,
    reason = "measured: plain #[inline] left a call per character in the unicode run writer"
)]
#[inline(always)]
fn char_width(c: char, cjk: bool) -> usize {
    let cp = c as u32;
    if (0x20..0x7F).contains(&cp) || (!cjk && (0xA0..0x0300).contains(&cp)) {
        1
    } else if (0x4E00..0xA000).contains(&cp) {
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
        2
    } else if (0x1F300..0x1F700).contains(&cp) {
        smp_emoji_width(cp)
    } else if (0x2600..0x27C0).contains(&cp) {
        bmp_emoji_width(cp, cjk)
    } else if (0x20000..0x2FA20).contains(&cp) || (0x30000..0x323B0).contains(&cp) {
        2
    } else if cjk {
        aterm_grapheme::char_width_cjk(c)
    } else {
        aterm_grapheme::char_width(c)
    }
}

/// Whether `c` continues a narrow run in [`TerminalHandler::write_unicode_bulk`]: a width-1 BMP
/// glyph outside the blocks its wide arm takes (U+3000-U+9FFF, Hangul syllables), and ASCII only
/// where GL prints it as itself (`ascii_passes`).
#[inline]
fn is_narrow_run_glyph(c: char, cjk: bool, ascii_passes: bool) -> bool {
    let cp = c as u32;
    (cp >= 0x80 || ascii_passes)
        && cp <= 0xFFFF
        && !(0x3000..0xA000).contains(&cp)
        && !(0xAC00..0xD7A4).contains(&cp)
        && char_width(c, cjk) == 1
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
        let left_of_margin = self.grid.cursor_left_of_margin();
        self.write_char_body(c);
        if left_of_margin {
            self.grid.note_print_from_left_of_margin();
        }
        self.grid.damage_selection_output(origin);
    }

    fn write_char_body(&mut self, c: char) {
        let translated = self.charset.translate(c);

        let width = char_width(translated, self.modes.ambiguous_width_double);

        if width == 0 {
            if is_c1_control(translated) {
                return;
            }
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

        if width == 2 && self.grid.cols() < 2 {
            // A screen one column wide has no room for a wide glyph: it prints as a narrow
            // space in the pen, as Ghostty's `print` writes it (xterm's `dotext` gives up on
            // the glyph), never a blank-and-wrap that leaves the cursor on the next row.
            self.write_char_core(' ', 1);
            return;
        }
        self.write_char_core(translated, width);
    }

    /// Bulk write path for runs of multi-byte UTF-8 characters and the printable ASCII the
    /// parser joins between them. The bulk loop ([`Self::write_unicode_run`]) takes what it
    /// can; from a character it does not take on, the rest goes as the parser's calls wrote it
    /// before runs joined ASCII: the characters up to the next ASCII one glyph at a time, that
    /// ASCII as `print_ascii_bulk` writes it (an extras pen keeps its colors in the row's ring,
    /// not a `CellExtra` per cell), and what follows as a run again, bracketed for selection
    /// damage on its own as its print was (`print_unicode_bulk` brackets the first).
    pub(super) fn write_unicode_bulk(&mut self, chars: &[char]) {
        let mut rest = chars;
        let mut origin = None;
        while !rest.is_empty() {
            let taken = self.write_unicode_run(rest);
            if let Some(origin) = origin {
                self.grid.damage_selection_output(origin);
            }
            let left = rest.get(taken..).unwrap_or(&[]);
            let (glyphs, left) =
                left.split_at(left.iter().position(char::is_ascii).unwrap_or(left.len()));
            for &c in glyphs {
                self.write_char(c);
            }
            let (ascii, left) = left.split_at(
                left.iter()
                    .position(|c| !c.is_ascii())
                    .unwrap_or(left.len()),
            );
            self.write_joined_ascii(ascii);
            rest = left;
            origin = Some(self.grid.output_damage_origin());
        }
    }

    /// The bulk loop of [`Self::write_unicode_bulk`]: writes `chars` from the first and returns
    /// how many it took, all of them or up to a character it does not take on (none under a
    /// mode that writes one glyph at a time).
    #[allow(
        clippy::too_many_lines,
        reason = "hot-path character dispatch with many optimized branches"
    )]
    fn write_unicode_run(&mut self, chars: &[char]) -> usize {
        // Preconditions: no insert mode, autowrap on, no style extras. VT52
        // addressing never reaches here (`print_unicode_bulk` routes it per
        // character). U+00A0-U+00FF may need GR-mapped translation when a
        // non-ASCII charset is designated on the GR G-set, so fall back then,
        // on a screen one column wide, where `write_char` prints a wide
        // glyph as a narrow space, and outside the margins, where a print can
        // step onto the left one.
        if self.modes.insert_mode
            || !self.modes.auto_wrap
            || self.transient.has_transient_extras
            || self.style.has_style_extras()
            || self.transient.last_combining_was_zwj
            || !self.charset.gr_is_passthrough()
            || self.grid.cols() < 2
            || self.grid.cursor_outside_margin_span()
        {
            return 0;
        }
        self.charset.clear_single_shift();

        let cjk = self.modes.ambiguous_width_double;
        // Whether the ASCII the parser joins to a run (the spaces between words) prints as
        // itself: GL is an ASCII G0. Read after the single shift is spent on the run's first
        // character, which is never ASCII.
        let ascii_passes = self.charset.is_ascii_passthrough();

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
                            self.bracket_mark_after_joined_ascii(chars, i);
                            self.transient.last_combining_was_zwj = false;
                            self.add_combining_to_previous_cell(c);
                            self.bracket_joined_ascii_after_mark(chars, i);
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
                    self.bracket_mark_after_joined_ascii(chars, i);
                    self.transient.last_combining_was_zwj = c == '\u{200D}';
                    self.add_combining_to_previous_cell(c);
                    self.bracket_joined_ascii_after_mark(chars, i);
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
            } else if cp < 0x80 && !ascii_passes {
                // Printable ASCII the parser joined to the run, under a GL set that translates
                // it: `print_ascii_bulk` writes it then.
                if let Some(g) = last_graphic.take() {
                    self.transient.last_graphic_char = Some(g);
                }
                return i;
            } else {
                let width = char_width(c, cjk);
                if width == 1 {
                    // A run of narrow BMP glyphs (the ASCII between words included) outside the
                    // wide blocks the first arm batches: one grid write per row.
                    let run_start = i;
                    i += 1;
                    while let Some(&next) = chars.get(i)
                        && is_narrow_run_glyph(next, cjk, ascii_passes)
                    {
                        i += 1;
                    }
                    self.write_narrow_run(&chars[run_start..i], colors, flags);
                    last_graphic = Some(chars[i - 1]);
                    self.transient.last_combining_was_zwj = false;
                    continue;
                }
                if width == 0 {
                    self.bracket_mark_after_joined_ascii(chars, i);
                    if is_c1_control(c) {
                        // It lands nowhere, but the joined ASCII after it still begins its own
                        // bracket, on a cell a lone mark before the control may have extended.
                        self.bracket_joined_ascii_after_mark(chars, i);
                        i += 1;
                        continue;
                    }
                    self.transient.last_combining_was_zwj = c == '\u{200D}';
                    self.add_combining_to_previous_cell(c);
                    if c == '\u{FE0F}' {
                        self.widen_previous_cell_for_vs16();
                    }
                    if c == '\u{FE0E}' {
                        self.narrow_previous_cell_for_vs15();
                    }
                    self.bracket_joined_ascii_after_mark(chars, i);
                    i += 1;
                    if self.transient.last_combining_was_zwj {
                        if let Some(g) = last_graphic.take() {
                            self.transient.last_graphic_char = Some(g);
                        }
                        return i;
                    }
                    continue;
                }
                self.grid.write_wide_autowrap_fast(c, colors, flags);
            }
            last_graphic = Some(c);
            self.transient.last_combining_was_zwj = false;
            i += 1;
        }

        if let Some(c) = last_graphic {
            self.transient.last_graphic_char = Some(c);
        }
        chars.len()
    }

    /// Bracket the mark at `chars[i]` as the parser's own print of it did before runs joined
    /// the ASCII before it: where that ASCII left the cursor at the row's end, the mark's print
    /// began on existing content and recorded the row as overwritten
    /// (`Grid::damage_selection_output`), which the run's one bracket, begun before the ASCII,
    /// does not.
    #[inline]
    fn bracket_mark_after_joined_ascii(&mut self, chars: &[char], i: usize) {
        if i > 0 && chars.get(i - 1).is_some_and(char::is_ascii) && self.grid.pending_wrap() {
            let origin = self.grid.output_damage_origin();
            self.grid.damage_selection_output(origin);
        }
    }

    /// Bracket the joined ASCII after the mark at `chars[i]` as its own `print_ascii_bulk` did
    /// before runs joined it: where the mark landed on the cell under the cursor (a row's first
    /// column, with no cell before it to join), that print began on the mark's cell and recorded
    /// the row as overwritten, which the run's one bracket, begun before the mark, does not.
    #[inline]
    fn bracket_joined_ascii_after_mark(&mut self, chars: &[char], i: usize) {
        if chars.get(i + 1).is_some_and(char::is_ascii) {
            let origin = self.grid.output_damage_origin();
            self.grid.damage_selection_output(origin);
        }
    }

    /// Write printable ASCII the parser joined to a run through `print_ascii_bulk`'s writer.
    fn write_joined_ascii(&mut self, ascii: &[char]) {
        let mut bytes = [0u8; 256];
        for part in ascii.chunks(bytes.len()) {
            for (byte, &c) in bytes.iter_mut().zip(part) {
                *byte = u8::try_from(c).unwrap_or(b'?');
            }
            self.write_ascii_print(bytes.get(..part.len()).unwrap_or(&[]));
        }
    }

    /// Write a run of narrow glyphs [`Self::write_unicode_bulk`] batched: one grid write per row,
    /// or one per glyph on the reference path (`Terminal::set_reference_print_path`).
    #[inline]
    fn write_narrow_run(
        &mut self,
        run: &[char],
        colors: crate::grid::PackedColors,
        flags: CellFlags,
    ) {
        #[cfg(any(test, debug_assertions))]
        if self.transient.reference_print_path {
            for &c in run {
                self.grid.write_narrow_autowrap_fast(c, colors, flags);
            }
            return;
        }
        self.grid.write_narrow_run_autowrap(run, colors, flags);
    }

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
                // The print edge is the right margin, as on the fast path: a wide glyph
                // with one column left before it wraps (xterm `dotext`, Ghostty `print`).
                let fill = crate::grid::Cell::bce_blank(colors);
                ecols = self.grid.pre_wrap_wide_ecols(fill);
            }
            if self.modes.insert_mode {
                if !self.modes.auto_wrap && self.grid.cursor_col().saturating_add(1) >= ecols {
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
        let transient = &*self.transient;
        for i in 0..cols {
            self.grid
                .extras_mut()
                .modify(aterm_grid::CellCoord::new(row, col + i), |extra| {
                    *extra = aterm_grid::CellExtra::default();
                    if let Some(ref hyperlink) = transient.current_hyperlink {
                        extra.set_hyperlink_data(Some(Arc::clone(hyperlink)));
                    }
                    if let Some(color) = transient.current_underline_color {
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
                });
        }

        if has_hyperlink {
            self.grid.enforce_hyperlink_limit();
        }
    }

    /// The cell a zero-width mark attaches to: [`Self::preceding_cell`], or when none exists the
    /// glyph under the cursor, a wide one's lead if the cursor stands on its spacer (a left margin
    /// can fall there).
    #[inline]
    fn combining_target_cell(&self) -> (u16, u16) {
        self.preceding_cell().unwrap_or_else(|| {
            let (row, col) = (self.grid.cursor_row(), self.grid.cursor_col());
            if col > 0 && self.grid.is_wide_continuation_at(row, col) {
                (row, col - 1)
            } else {
                (row, col)
            }
        })
    }

    /// The glyph a zero-width mark continues: the parked one under a pending wrap, else the one
    /// left of the cursor, and a wide glyph's lead rather than its spacer.
    ///
    /// A cursor a CR or a motion put on column 0 or the left margin has none, on a row that
    /// continues the one above too (xterm's `char_was_written` cleared gives the cursor's own cell
    /// there, as Alacritty's zero-width push does). One a print or a rewrap left there still follows
    /// its glyph ([`aterm_grid::Grid::cursor_follows_glyph`]): the one just left of the margin, or
    /// the last on the row above.
    fn preceding_cell(&self) -> Option<(u16, u16)> {
        let row = self.grid.cursor_row();
        let col = self.grid.cursor_col();

        let (target_row, target_col) = if self.grid.pending_wrap() {
            (row, col)
        } else if self.grid.cursor_starts_its_span() && !self.grid.cursor_follows_glyph() {
            return None;
        } else if col > 0 {
            (row, col - 1)
        } else if self.grid.row(row).is_some_and(aterm_grid::Row::is_wrapped) {
            let above = row.checked_sub(1)?;
            let end = self
                .grid
                .row(above)?
                .len()
                .min(self.grid.effective_cols_for_row(above));
            (above, end.checked_sub(1)?)
        } else {
            return None;
        };

        if target_col > 0 && self.grid.is_wide_continuation_at(target_row, target_col) {
            Some((target_row, target_col - 1))
        } else {
            Some((target_row, target_col))
        }
    }

    fn add_combining_to_previous_cell(&mut self, combining: char) {
        let (row, col) = self.combining_target_cell();
        self.grid.cell_extra_mut(row, col).add_combining(combining);
        if let Some(r) = self.grid.row_mut(row) {
            r.update_len(col.saturating_add(1));
        }
        self.grid.mark_content_cell(row, col);
    }

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

    /// Where a selector's change of width has to stop for the glyph at `(row, col)`, one past the
    /// last column it may use: the right margin of a glyph inside it, as a wide glyph's own write
    /// stops there, else the row's end, which a double-width row keeps at its usable half.
    fn cluster_line_end(&self, row: u16, col: u16) -> u16 {
        let line_end = self.grid.effective_cols_for_row(row);
        let right = self.grid.horizontal_margins().right;
        if self.grid.has_horizontal_margins() && col <= right {
            right.saturating_add(1).min(line_end)
        } else {
            line_end
        }
    }

    fn widen_previous_cell_for_vs16(&mut self) {
        let Some((row, col)) = self.preceding_cell() else {
            return;
        };
        // The cursor's advance past the spacer has no meaning from a row a rewrap left above it.
        if row != self.grid.cursor_row() {
            return;
        }

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

        if cont_col >= self.cluster_line_end(row, col) {
            return;
        }

        let wide_base =
            crate::grid::Cell::from_raw_parts(char_data, colors, base_flags.union(CellFlags::WIDE));
        let cont_cell = crate::grid::Cell::from_raw_parts(
            ' ' as u16,
            colors.without_extras_flag(),
            base_flags.wide_continuation_of(),
        );
        // A glyph the spacer cuts in half goes whole: its other half takes the writer's BCE blank,
        // as a print over half a wide glyph blanks it.
        let fill = Cell::bce_blank(self.style.cached_colors());
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
                            row_data.set(orphan_col, fill);
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
            if fill.colors().bg_is_rgb() {
                self.grid.fill_bce_rgb_range(row, oc, oc.saturating_add(1));
            }
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
        if cont_col >= self.cluster_line_end(row, col) {
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

/// Whether a character is a C1 control (U+0080-U+009F), which the UTF-8 decoder hands the print
/// path. It takes no column and is no mark, so it changes nothing: a cell never holds a control.
#[inline]
const fn is_c1_control(c: char) -> bool {
    matches!(c as u32, 0x80..=0x9F)
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

/// Whether `c` is eligible for VS16 emoji presentation widening.
#[inline]
pub(crate) fn is_vs16_emoji_capable(c: char) -> bool {
    aterm_grapheme::is_vs16_emoji_capable(c)
}

#[cfg(test)]
#[path = "handler_write_tests.rs"]
mod tests;
