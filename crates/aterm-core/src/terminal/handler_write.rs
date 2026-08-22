// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Character write-path helpers for terminal rendering.
//!
//! This module contains the hot path for writing translated characters,
//! REP writes, and combining-character/ZWJ continuation behavior.

use std::sync::Arc;

use crate::grid::{Cell, CellFlags, row_u16};

use super::TerminalHandler;

/// Fast character width lookup with multi-tier fast-path.
///
/// Tier 1: ASCII (0x20-0x7E) — always width 1, covers ~90% of terminal content.
/// Tier 2: Latin-1 Supplement through Spacing Modifiers (0xA0-0x02FF) — all width 1.
/// Tier 3: CJK Unified Ideographs (U+4E00-U+9FFF) and Hangul Syllables
///         (U+AC00-U+D7A3) — uniformly width 2. The rest of the CJK block
///         (U+3000-U+4DFF) is NOT uniformly wide — it mixes wide, narrow,
///         East-Asian-Ambiguous (config-dependent), and zero-width combining
///         codepoints — so it defers to the authoritative `aterm_grapheme`
///         table (honoring the `cjk` flag) to stay in lockstep with the
///         reflow/materialize/fill paths.
/// Tier 4: SMP Emoji (U+1F300-U+1F6FF) — O(1) bitmap lookup.
///         BMP Emoji (U+2600-U+27BF) — O(1) bitmap lookup.
///         CJK Extension B-H + Compat Ideographs Supp (U+20000-U+2FA1F, U+30000-U+323AF) — width 2.
/// Tier 5: Full aterm_grapheme::char_width lookup for everything else.
///         Uses `width_cjk()` when `cjk` is true for East Asian Ambiguous chars.
#[inline]
fn char_width(c: char, cjk: bool) -> usize {
    let cp = c as u32;
    // Tier 2 fast path only safe when NOT in CJK mode — the 0xA0-0x02FF range
    // contains many EA Width "Ambiguous" characters (°, §, ±, ×, ÷, etc.)
    // that should be width 2 in CJK mode.
    if (0x20..0x7F).contains(&cp) || (!cjk && (0xA0..0x0300).contains(&cp)) {
        1
    } else if (0x4E00..0xA000).contains(&cp) {
        // CJK Unified Ideographs (U+4E00-U+9FFF): uniformly East Asian Wide in
        // both ambiguous-width modes (verified exhaustively against the
        // authoritative table by the `fast_path_width_matches_table_cjk_block`
        // parity test).
        2
    } else if (0x3000..0x4E00).contains(&cp) {
        // CJK Symbols & Punctuation, Kana, Bopomofo, Hangul Compatibility Jamo,
        // Kanbun, Yijing Hexagrams (U+4DC0-U+4DFF, East Asian Wide), Enclosed
        // CJK, etc. This sub-block is NOT uniformly wide: it mixes wide, narrow
        // (e.g. U+303F and unassigned gaps), East-Asian-Ambiguous codepoints
        // (U+3248-U+324F, whose width depends on `cjk`), and zero-width
        // combining marks (U+302A-302D, U+3099-309A). Defer to the authoritative
        // table so the print path matches the reflow/materialize/fill paths.
        if cjk {
            aterm_grapheme::char_width_cjk(c)
        } else {
            aterm_grapheme::char_width(c)
        }
    } else if (0xAC00..0xD7A4).contains(&cp) {
        2 // Hangul Syllables — always width 2
    } else if (0x1F300..0x1F700).contains(&cp) {
        // SMP Emoji blocks (Misc Symbols, Emoticons, Ornamental Dingbats, Transport).
        smp_emoji_width(cp)
    } else if (0x2600..0x27C0).contains(&cp) {
        // BMP Misc Symbols + Dingbats (✨ U+2728, ⚡ U+26A1, zodiac, etc.).
        bmp_emoji_width(cp)
    } else if (0x20000..0x2FA20).contains(&cp) || (0x30000..0x323B0).contains(&cp) {
        // CJK Extension B through Compat Ideographs Supp (U+20000-U+2FA1F): all width 2.
        // CJK Extensions G (U+30000-U+3134F) and H (U+31350-U+323AF): all width 2.
        2
    } else if cjk {
        aterm_grapheme::char_width_cjk(c)
    } else {
        aterm_grapheme::char_width(c)
    }
}

/// Bitmap of width-2 codepoints in U+1F300-U+1F6FF (128 bytes, L1-cache friendly).
///
/// Generated from Unicode East Asian Width tables. Each bit = one codepoint:
/// set = width 2, clear = width 1/0. Covers Miscellaneous Symbols and
/// Pictographs, Emoticons, Ornamental Dingbats, and Transport/Map Symbols.
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
        // Rare text-presentation symbols: fall through for correctness.
        // SAFETY: cp is guaranteed to be a valid Unicode codepoint (U+1F300-U+1F6FF).
        aterm_grapheme::char_width(unsafe { char::from_u32_unchecked(cp) }).max(1)
    }
}

/// Bitmap of width-2 codepoints in U+2600-U+27BF (56 bytes).
///
/// Covers Miscellaneous Symbols (U+2600-U+26FF) and Dingbats (U+2700-U+27BF).
/// Only 10% of codepoints are width 2 (zodiac signs, ✨, ⚡, misc emoji).
#[rustfmt::skip]
static BMP_EMOJI_WIDTH2: [u8; 56] = [
    0x00, 0x00, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0x0F, 0x00, 0x00, 0x00, 0x00, 0x80,
    0x00, 0x00, 0x08, 0x00, 0x02, 0x0C, 0x00, 0x60, 0x30, 0x40, 0x10, 0x00, 0x00, 0x04, 0x2C, 0x24,
    0x20, 0x0C, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x50, 0xB8, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0xE0, 0x00, 0x00, 0x00, 0x01, 0x80,
];

/// O(1) bitmap lookup for BMP emoji width (U+2600-U+27BF).
#[inline]
fn bmp_emoji_width(cp: u32) -> usize {
    let idx = (cp - 0x2600) as usize;
    if BMP_EMOJI_WIDTH2[idx / 8] & (1 << (idx % 8)) != 0 {
        2
    } else {
        // Fallback to char_width for bitmap misses, matching smp_emoji_width().
        // SAFETY: cp is guaranteed to be a valid Unicode codepoint (U+2600-U+27BF).
        aterm_grapheme::char_width(unsafe { char::from_u32_unchecked(cp) }).max(1)
    }
}

impl TerminalHandler<'_> {
    /// Write a character to the grid with current style.
    ///
    /// SPEC: this glyph-print path is BOTH the `WriteMain` and the `Scribble` action
    /// of the external `AltScreen.tla` model (TRUST_NATIVE_TLA Phase 2): writing a
    /// cell to the ACTIVE buffer. Which spec action it is depends only on which
    /// buffer is active (`active = "main"` ⇒ `WriteMain`, `active = "alt"` ⇒
    /// `Scribble`) — there is one print path, exactly as the spec gates the two
    /// actions on `active`. The spec's `MainRestoredAfterRoundTrip` invariant
    /// (Buggy=FALSE: alt scribbles land in the ISOLATED alt buffer, never aliasing
    /// main) is what the Tier-1 conformance asserts by driving real input on each
    /// buffer and reading the main cells back unchanged across the round-trip.
    // PROJECTION (TRUST_VACUITY_GATE §2.2 / finding 2): a `write_char` to the main
    // vs the alt buffer projects onto the spec's `mainCell`/`altCell` + `cursor` of
    // `<<active, mainCell, altCell, cursor, savedCursor, entered, mainSaved>>` — the
    // same `aterm_core::terminal::project_altscreen` projection `conformance_altscreen.rs`
    // drives. L2 requires the projection NAME be present (Trust does not execute it).
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "alt_screen",
            action = "WriteMain",
            project = "aterm_core::terminal::project_altscreen"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "alt_screen",
            action = "Scribble",
            project = "aterm_core::terminal::project_altscreen"
        )
    )]
    pub(super) fn write_char(&mut self, c: char) {
        // Translate character through the active character set
        let translated = self.charset.translate(c);

        // Capture for CopyToClipboard mode
        if let Some(state) = self.clipboard.copy_state.as_mut() {
            state.push(translated);
        }

        let width = char_width(translated, self.modes.ambiguous_width_double);

        if width == 0 {
            // Track whether this combining character is a ZWJ for fast-path skipping.
            self.transient.last_combining_was_zwj = translated == '\u{200D}';
            self.add_combining_to_previous_cell(translated);
            // VS16 (U+FE0F): emoji presentation selector widens eligible base
            // characters from 1 cell to 2 cells, matching kitty/WezTerm/foot.
            if translated == '\u{FE0F}' {
                self.widen_previous_cell_for_vs16();
            }
            // VS15 (U+FE0E): text presentation selector narrows emoji from
            // 2 cells to 1 cell, the inverse of VS16.
            if translated == '\u{FE0E}' {
                self.narrow_previous_cell_for_vs15();
            }
            return;
        }

        // Store for REP (CSI b): only track width > 0 graphic characters.
        // Combining marks and ZWJ are not "preceding graphic characters" per
        // ECMA-48 §8.3.103 and should not be repeated by REP.
        // Track the RAW received char, not the translated glyph: xterm
        // CASE_REP re-translates `lastchar` through the CURRENT GL charset
        // (dotext(xw, screen->gsets[curgl], ...)), so a charset designation
        // between the print and the REP changes the repeated glyph.
        self.transient.last_graphic_char = Some(c);

        // Emoji skin tone modifiers (U+1F3FB-U+1F3FF) should combine with the
        // preceding emoji base rather than rendering as separate 2-cell characters.
        if is_emoji_skin_tone_modifier(translated)
            && self.try_combine_skin_tone_modifier(translated)
        {
            return;
        }

        // Regional-indicator pairs (U+1F1E6-U+1F1FF) form ONE flag glyph: combine
        // the 2nd RI of a pair into the 1st RI's cell so `🇺🇸` is a single 2-cell
        // grapheme. The colour font has a bitmap for the PAIR, not single RIs.
        if is_regional_indicator(translated) && self.try_combine_regional_indicator(translated) {
            return;
        }

        // ZWJ sequence continuation: combine with previous cell for emoji sequences.
        // Fast-path: skip the expensive grid lookup unless the last combining char was ZWJ.
        if self.transient.last_combining_was_zwj && self.should_combine_with_previous_zwj() {
            self.add_combining_to_previous_cell(translated);
            return;
        }
        self.transient.last_combining_was_zwj = false;

        self.write_char_core(translated, width);
    }

    /// Bulk write path for runs of non-ASCII characters.
    ///
    /// Called by the parser when 2+ consecutive multi-byte UTF-8 sequences are
    /// decoded. Checks preconditions once and dispatches to a tight inner loop,
    /// skipping per-character charset translate, clipboard capture, char_width,
    /// ZWJ tracking, and style/extras computation.
    ///
    /// Falls back to per-character `write_char` when preconditions aren't met
    /// (VT52, insert mode, no autowrap, active clipboard, pending ZWJ, extras).
    #[allow(
        clippy::too_many_lines,
        reason = "hot-path character dispatch with many optimized branches"
    )]
    pub(super) fn write_unicode_bulk(&mut self, chars: &[char]) {
        // Precondition: must NOT be in VT52 cursor addressing, insert mode,
        // no-autowrap, or clipboard capture mode, and must not have style extras.
        if self.transient.vt52_cursor_state != super::Vt52CursorState::None
            || self.modes.insert_mode
            || !self.modes.auto_wrap
            || self.clipboard.copy_state.is_some()
            || self.transient.has_transient_extras
            || self.style.has_style_extras()
            || self.transient.last_combining_was_zwj
        {
            for &c in chars {
                self.write_char(c);
            }
            return;
        }

        // Non-ASCII chars bypass charset translation for the GL range (>= 0x100).
        // However, characters in U+00A0-U+00FF may need GR-mapped translation
        // when a non-ASCII charset is designated on the GR-mapped G-set (#7546).
        // Fall back to per-char processing when GR translation is active.
        if !self.charset.gr_is_passthrough() {
            for &c in chars {
                self.write_char(c);
            }
            return;
        }
        // Clear single_shift once for the entire batch.
        self.charset.clear_single_shift();

        // Cache ambiguous-width mode for the entire bulk run.
        let cjk = self.modes.ambiguous_width_double;

        // DECSLRM (a non-full left/right margin span) forces the wide-run
        // batchers off, because they are NOT grid-identical to the per-char
        // path under one: `Grid::margin_clamped_ecols` clamps the row's column
        // limit to `margins.right + 1` only while the cursor sits INSIDE
        // [left, right], and the batchers derive that limit ONCE per row while
        // the per-char writers re-derive it before EVERY glyph. With the cursor
        // placed left of `margins.left` (DECSLRM itself homes to column 0), a
        // wide run that crosses into the span therefore keeps the UNCLAMPED
        // limit for the whole row in the batched form: it writes straight past
        // `margins.right`, blanks a different BCE wrap tail, and wraps in a
        // different column than the identical bytes fed one at a time. That is
        // exactly the chunked-process equivalence invariant, so the batchers
        // are used only where they provably agree. The predicate is the same
        // one the grid uses to arm its margin-aware slow paths
        // (`has_horizontal_margins`), and it is now READ from the grid's own
        // maintained flag rather than re-derived here — zero per glyph, and one
        // bool load rather than two accessor reads plus a compare per bulk call.
        // That re-derivation measured: it is the residual ~1.7% on
        // `engine_throughput/cjk`, which is the only corpus that reaches this
        // function (ASCII takes the disjoint `print_ascii_bulk` lane).
        let margins_active = self.grid.has_horizontal_margins();

        // Pre-compute style state once for the entire run.
        let colors = self.style.cached_colors();
        let flags = if self.style.protected {
            self.style.flags.union(CellFlags::PROTECTED)
        } else {
            self.style.flags
        };
        // Pre-compute complex flags for non-BMP emoji (hoisted from inner loop).
        let complex_flags = flags.union(CellFlags::COMPLEX);

        // Track last graphic char across the bulk run for REP (CSI b).
        // Only updated for width > 0 characters.
        let mut last_graphic: Option<char> = None;

        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let cp = c as u32;

            // Width classification: inline the hot tiers of char_width.
            // CJK Unified Ideographs (U+4E00-U+9FFF) and Hangul (U+AC00-U+D7A3)
            // are uniformly width 2; the lower CJK block (U+3000-U+4DFF) is
            // mixed-width and defers to the table (see char_width).
            // Latin Supplement through Spacing Modifiers (U+00A0-U+02FF) are width 1.
            // Everything else goes through the per-character slow path.
            //
            // The wide-run batcher below LEADS on any of the three width-2
            // families it can extend across — CJK Unified Ideographs, the
            // width-2 subset of U+3000-U+4DFF, and Hangul Syllables — so the
            // lead gate here matches the run-extension predicate below exactly.
            // (A Hangul lead used to fall through to a per-syllable write, which
            // meant pure-Hangul runs — i.e. all Korean text — never batched,
            // while Hangul in a CJK-led run always did.) The grid is the same
            // either way ONLY because `margins_active` sends the run down the
            // per-glyph writer whenever DECSLRM is armed; outside that,
            // `write_wide_run_autowrap` is the batched form of repeated
            // `write_wide_autowrap_fast`. See the `margins_active` note above.
            if (0x3000..0xA000).contains(&cp) || (0xAC00..0xD7A4).contains(&cp) {
                // CJK Unified Ideographs (U+4E00-U+9FFF) and Hangul Syllables
                // (U+AC00-U+D7A3) are uniformly wide; the lower part of the CJK
                // block (U+3000-U+4DFF) is mixed-width, so classify those
                // individually via the authoritative `char_width` (which honors
                // `cjk`) and only fold genuine width-2 cells into the batched
                // wide run. This keeps the print path in lockstep with the
                // table-based reflow/materialize/fill paths. The test is spelled
                // as a closed range (not `cp < 0x4E00`) so it stays correct if a
                // block below U+3000 is ever added to the lead gate above.
                if (0x3000..0x4E00).contains(&cp) {
                    match char_width(c, cjk) {
                        0 => {
                            // Zero-width CJK combining marks (U+302A-302D tone
                            // marks, U+3099-309A voicing) — attach to prev cell.
                            self.transient.last_combining_was_zwj = false;
                            self.add_combining_to_previous_cell(c);
                            i += 1;
                            continue;
                        }
                        1 => {
                            // Narrow codepoints in the block (e.g. U+303F, the
                            // ambiguous run U+3248-U+324F in non-CJK mode, and
                            // unassigned-narrow gaps) — place as a single cell.
                            self.grid.write_narrow_autowrap_fast(c, colors, flags);
                            last_graphic = Some(c);
                            self.transient.last_combining_was_zwj = false;
                            i += 1;
                            continue;
                        }
                        // Width 2 — fall through to the wide-run batching below.
                        _ => {}
                    }
                }
                // BMP width-2: find the run of consecutive width-2 chars drawn
                // from the same three families as the lead gate — CJK Unified
                // Ideographs, the width-2 subset of U+3000-U+4DFF, and Hangul
                // Syllables.
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
                // Batch write the entire CJK/Hangul run — unless DECSLRM is
                // armed, where only the per-glyph writer re-derives the
                // margin clamp per column and so matches fragmented input.
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
                // Non-BMP (emoji, math symbols, etc.)
                let width = char_width(c, cjk);
                if width == 0 {
                    self.transient.last_combining_was_zwj = c == '\u{200D}';
                    self.add_combining_to_previous_cell(c);
                    i += 1;
                    continue;
                }
                if width == 2 {
                    // ZWJ continuation: combine with previous cell for emoji sequences.
                    if self.transient.last_combining_was_zwj
                        && self.should_combine_with_previous_zwj()
                    {
                        self.add_combining_to_previous_cell(c);
                        self.transient.last_combining_was_zwj = false;
                        i += 1;
                        continue;
                    }
                    // Skin tone modifiers combine with previous emoji base.
                    if is_emoji_skin_tone_modifier(c) && self.try_combine_skin_tone_modifier(c) {
                        self.transient.last_combining_was_zwj = false;
                        i += 1;
                        continue;
                    }
                    // Regional-indicator pairs form one flag glyph (see write_char).
                    if is_regional_indicator(c) && self.try_combine_regional_indicator(c) {
                        self.transient.last_combining_was_zwj = false;
                        i += 1;
                        continue;
                    }
                    self.transient.last_combining_was_zwj = false;
                    // Find run of consecutive width-2 chars (both BMP and non-BMP)
                    // for batching. Extending runs to include BMP emoji like
                    // ✨ U+2728 and ⚡ U+26A1 avoids per-char dispatch overhead
                    // when they appear adjacent to SMP emoji.
                    let run_start = i;
                    i += 1;
                    while i < chars.len() {
                        let c2 = chars[i];
                        let cp2 = c2 as u32;
                        // Break before a skin-tone modifier or a regional
                        // indicator so each is processed individually and can
                        // combine into the preceding cell next iteration.
                        if is_emoji_skin_tone_modifier(c2) || is_regional_indicator(c2) {
                            break;
                        }
                        // The two CJK SMP ranges below are uniformly East-Asian
                        // Wide, so they stay as O(1) fast-path extends. The
                        // emoji/symbols block 0x1F300..0x1FB00 is NOT uniform —
                        // it interleaves width-2 emoji with 948 width-1 symbols
                        // (Alchemical, Ornamental Dingbats, Geometric Shapes
                        // Extended, Chess, U+1F321, …) — so it must go through
                        // the char_width catch-all, which still admits the
                        // genuine width-2 emoji while letting width-1 symbols
                        // break the run. Mis-batching them as wide here desynced
                        // the bulk path from write_char/reflow/materialize and
                        // broke chunked-process equivalence.
                        if (0x20000..0x2FA20).contains(&cp2)
                            || (0x30000..0x323B0).contains(&cp2)
                            || char_width(c2, cjk) == 2
                        {
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    // Batch write mixed wide run — handles BMP and non-BMP.
                    // `write_mixed_wide_run_autowrap` derives its margin clamp
                    // once per row exactly like `write_wide_run_autowrap`, so
                    // it carries the same DECSLRM divergence and is gated the
                    // same way; the fallback mirrors `write_char_core`'s
                    // width-2 fast path per glyph (COMPLEX flags for non-BMP).
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
                // Rare: width-1 non-BMP (some math symbols)
                self.write_char_core(c, width);
            } else if !cjk && (0xA0..0x300).contains(&cp) {
                // Latin Supplement through Spacing Modifiers — width 1 when not CJK.
                // Most common non-ASCII in European text. Skip char_width().
                // In CJK mode, many chars here are ambiguous-width → fall through.
                self.grid.write_narrow_autowrap_fast(c, colors, flags);
            } else {
                // BMP non-CJK: compute width and use standard path
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
                    // A pending ZWJ must fold the FOLLOWING glyph into the
                    // previous cell (see `write_char`). The width-1 bulk
                    // branches below do not honor that, so once a ZWJ is
                    // pending, finish the run on the per-char reference path —
                    // identical to how fragmented input is processed, which is
                    // what keeps the fold a pure function of the byte log.
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
                // Width-1 BMP chars (Greek, Cyrillic, etc.)
                // or width-2 BMP chars outside CJK main blocks
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

        // Update last_graphic_char for REP (CSI b): only width > 0 chars.
        if let Some(c) = last_graphic {
            self.transient.last_graphic_char = Some(c);
        }
    }

    /// Shared write path: insert, write at cursor, apply extras, advance.
    ///
    /// Uses split write/advance primitives so that extras (hyperlinks,
    /// non-BMP overflow, underline colors, RGB) are applied at the correct
    /// cursor position BEFORE the cursor advances and potentially triggers
    /// an autowrap scroll that shifts the written row.
    fn write_char_core(&mut self, c: char, width: usize) {
        // Read colors/flags directly from CurrentStyle — avoids StyleTable lookup.
        // CurrentStyle is already kept in sync by `apply_style_change()` (and its
        // narrower siblings) on SGR changes — there is no style table to consult.
        let mut flags = if self.style.protected {
            self.style.flags.union(CellFlags::PROTECTED)
        } else {
            self.style.flags
        };

        // Non-BMP characters (emoji, math symbols, etc.) need overflow storage.
        // Pre-set the COMPLEX flag so readers know to look up the actual codepoint.
        let is_non_bmp = (c as u32) > 0xFFFF;
        if is_non_bmp {
            flags = flags.union(CellFlags::COMPLEX);
        }

        // Pre-compute whether this character needs CellExtras HashMap overflow.
        // Non-BMP complex chars use the dense ring buffer (O(1) flat array)
        // instead of the HashMap, so they don't need the HAS_EXTRAS flag.
        // Uses cached booleans to avoid 5 per-character Option/bitfield checks.
        // Both flags are updated at mutation time (SGR, OSC 8) — not per character.
        let needs_style_extras =
            self.transient.has_transient_extras || self.style.has_style_extras();

        // Use cached packed colors — computed once per SGR change, not per character.
        // Only set HAS_EXTRAS for style extras (HashMap). Non-BMP uses the ring
        // buffer and doesn't need the flag — readers check COMPLEX independently.
        let colors = if needs_style_extras {
            self.style.cached_colors().with_extras_flag()
        } else {
            self.style.cached_colors()
        };

        // FAST PATH: Wide char with autowrap, no insert mode, no style extras.
        // Combined pre-wrap + write + damage + advance in a single Grid call,
        // eliminating 4-6 separate method calls and redundant bounds checks.
        if width == 2 && self.modes.auto_wrap && !self.modes.insert_mode && !needs_style_extras {
            if is_non_bmp {
                // Non-BMP emoji: combined write + ring-buffer codepoint store (no Arc)
                self.grid.write_emoji_autowrap_fast(c, colors, flags);
            } else {
                // BMP CJK: combined write (no ring buffer needed)
                self.grid.write_wide_autowrap_fast(c, colors, flags);
            }
            return;
        }

        // Resolve deferred wrap before writing the next character.
        // This matches xterm behavior: the wrap only happens when the next
        // printable character arrives, not when the last column is filled.
        // xterm consumes do_wrap at print time UNCONDITIONALLY and wraps
        // only when WRAPAROUND is set (charproc.c dotext: `do_wrap = False;
        // if (flags & WRAPAROUND) WrapLine;`) — with autowrap off the flag
        // is discarded and the write overstrikes the margin column (it is
        // re-armed by the no-wrap advance below when it fills the margin).
        if self.modes.auto_wrap {
            self.grid.resolve_pending_wrap();
        } else {
            self.grid.set_pending_wrap(false);
        }

        // Phase 1: Write character at cursor (no cursor advance yet).
        // Wide chars use _ecols variants to compute effective_cols once instead
        // of 3 separate ring-buffer lookups (pre-wrap, write, advance).
        let (did_write, ecols) = if width == 2 {
            let mut ecols = self.grid.effective_cols_for_current_row();
            if self.modes.auto_wrap {
                // Pass a BCE blank so a wide glyph wrapping off the last column
                // blanks the skipped cell with the current background (xterm).
                let fill = crate::grid::Cell::bce_blank(colors);
                ecols = self.grid.pre_wrap_wide_ecols(ecols, fill);
            }
            // In insert mode without auto-wrap, check that the wide char fits
            // before inserting blanks. Otherwise insert_chars shifts content
            // right but the write fails, leaving a spurious blank (#7483).
            if self.modes.insert_mode {
                if !self.modes.auto_wrap && self.grid.cursor_col().saturating_add(1) >= ecols {
                    // Wide char doesn't fit — no-op, matching xterm.
                    return;
                }
                // IRM insert must respect horizontal margins like ICH (#7580).
                self.grid
                    .insert_chars_margin(row_u16(width), self.modes.left_right_margin_mode);
            }
            let ok = self
                .grid
                .write_wide_char_at_cursor_packed_ecols(c, colors, flags, ecols);
            (ok, ecols)
        } else {
            if self.modes.insert_mode {
                // IRM insert must respect horizontal margins like ICH (#7580).
                self.grid
                    .insert_chars_margin(row_u16(width), self.modes.left_right_margin_mode);
            }
            self.grid.write_char_at_cursor_packed(c, colors, flags);
            (true, 0) // ecols unused for width-1
        };

        if !did_write {
            return;
        }

        // Phase 2: Apply extras at the correct position.
        // The cursor is still on the written character, so cursor_row/col
        // are the actual write coordinates — unaffected by any future scroll.
        let row = self.grid.cursor_row();
        let col = self.grid.cursor_col();

        // Apply extras at the correct position.
        // Non-BMP: store codepoint in ring buffer (O(1) flat array, ~1ns) instead of
        // HashMap entry (~15ns). No Arc allocation. The COMPLEX flag was set in Phase 1.
        if is_non_bmp {
            self.grid.set_complex_char_ring(row, col, c);
        }
        // Style extras (hyperlinks, underline color, RGB, extended flags)
        // still use the HashMap via the preflagged path.
        if needs_style_extras {
            self.apply_cell_extras_preflagged(row, col, width);
        }

        // Phase 3: Advance cursor (may trigger autowrap + scroll).
        // Wide chars reuse the pre-computed ecols to avoid a third ring-buffer lookup.
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
    ///
    /// Uses `cell_extra_mut_preflagged` — the caller must have already set
    /// the HAS_EXTRAS bit in the cell's PackedColors during the write step.
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

        // Apply to primary cell and optional wide continuation
        let cols = if width == 2 && col + 1 < self.grid.cols() {
            2
        } else {
            1
        };
        for i in 0..cols {
            // HAS_EXTRAS flag already set in PackedColors during write step —
            // skip the redundant ring-buffer row_index lookup.
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

        // Enforce hyperlink entry limit to prevent memory exhaustion from
        // OSC 8 spam with unique URLs (#7172). The check is O(1) when under
        // the limit (just a HashMap::len() comparison).
        if has_hyperlink {
            self.grid.enforce_hyperlink_limit();
        }
    }

    /// Find the previous effective cell, skipping wide continuation cells.
    ///
    /// Returns `None` at position (0, 0) where no previous cell exists.
    /// Handles column 0 by wrapping to the last column of the previous row
    /// (for combining chars at the start of a wrapped line). If the target
    /// is a wide continuation cell, returns the main wide cell instead.
    ///
    /// When `pending_wrap` is set, the cursor sits ON the last written
    /// character (not one past it), so the target is the cursor cell itself.
    fn previous_effective_cell(&self) -> Option<(u16, u16)> {
        let row = self.grid.cursor_row();
        let col = self.grid.cursor_col();

        if col == 0 && row == 0 && !self.grid.pending_wrap() {
            return None;
        }

        // When pending_wrap is set, the cursor is ON the last written char.
        // Without pending_wrap, the cursor is one past the last written char.
        let (target_row, target_col) = if self.grid.pending_wrap() {
            (row, col)
        } else if col > 0 {
            (row, col - 1)
        } else {
            // Only cross line boundary if current row is a soft-wrapped
            // continuation. Hard newlines mean column 0 has no predecessor
            // on the previous line.
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

        // Skip a wide continuation cell to land on its main cell. Use the
        // context-aware check: the raw `Cell::is_wide_continuation()` shares
        // bit 10 with PROTECTED, so a DECSCA-protected base char would be
        // misread as a spacer and a following combining mark / VS16 would attach
        // to the wrong cell.
        let (final_row, final_col) =
            if target_col > 0 && self.grid.is_wide_continuation_at(target_row, target_col) {
                (target_row, target_col - 1)
            } else {
                (target_row, target_col)
            };

        Some((final_row, final_col))
    }

    /// Add a combining character to the previous cell.
    ///
    /// Combining characters (like accents) attach to the base character in the
    /// previous cell. For wide characters, we attach to the main cell (not the
    /// continuation).
    fn add_combining_to_previous_cell(&mut self, combining: char) {
        let Some((row, col)) = self.previous_effective_cell() else {
            return;
        };
        self.grid.cell_extra_mut(row, col).add_combining(combining);
        self.grid.damage_mut().mark_cell(row, col);
    }

    /// Check if the previous cell ends with ZWJ (Zero Width Joiner).
    ///
    /// Used to detect ZWJ sequences like emoji family sequences where multiple
    /// emoji should render as a single grapheme (e.g., 👨‍💻 = 👨 + ZWJ + 💻).
    fn should_combine_with_previous_zwj(&self) -> bool {
        const ZWJ: char = '\u{200D}';

        let Some((row, col)) = self.previous_effective_cell() else {
            return false;
        };

        self.grid
            .cell_extra(row, col)
            .and_then(|extra| extra.combining().last().copied())
            == Some(ZWJ)
    }

    /// Widen the previous cell from 1-cell to 2-cell when VS16 (U+FE0F) follows
    /// an emoji-capable base character.
    ///
    /// Modern terminals (kitty, WezTerm, foot) treat VS16 as an emoji presentation
    /// selector that converts text-presentation emoji (width 1) to emoji-presentation
    /// (width 2). This function:
    /// 1. Checks if the previous cell's base char is emoji-capable
    /// 2. Sets the WIDE flag on the base cell
    /// 3. Writes a WIDE_CONTINUATION spacer in the next column
    /// 4. Advances the cursor to account for the extra column consumed
    fn widen_previous_cell_for_vs16(&mut self) {
        let Some((row, col)) = self.previous_effective_cell() else {
            return;
        };

        // Already wide — nothing to do.
        let Some(cell) = self.grid.cell(row, col) else {
            return;
        };
        if cell.is_wide() {
            return;
        }

        // Read the base character and check if it's emoji-capable.
        // For COMPLEX cells (non-BMP), cell.char() returns U+FFFD — resolve
        // the real codepoint from the overflow table (#7457).
        let base_char = if cell.is_complex() {
            self.grid.resolved_char(row, col).unwrap_or('\u{FFFD}')
        } else {
            cell.char()
        };
        if !is_vs16_emoji_capable(base_char) {
            return;
        }

        // Snapshot the base cell's raw data so we can reconstruct it with WIDE.
        let char_data = cell.char_data();
        let colors = cell.colors();
        let base_flags = cell.flags();

        // Determine where the continuation cell goes. The cursor is already
        // past the base cell (at col+1) unless pending_wrap is set.
        let cont_col = col + 1;

        // Check that the continuation column is within bounds.
        // Use effective_cols_for_row to handle DECDWL lines (#7457).
        if cont_col >= self.grid.effective_cols_for_row(row) {
            return;
        }

        // Rebuild the base cell with the WIDE flag added and write it via
        // Row::set(), which sets HAS_WIDE_CHARS and DIRTY on the row.
        let wide_base =
            crate::grid::Cell::from_raw_parts(char_data, colors, base_flags.union(CellFlags::WIDE));
        let cont_cell =
            crate::grid::Cell::from_raw_parts(' ' as u16, colors, CellFlags::WIDE_CONTINUATION);
        // If the continuation column currently holds the first half of a
        // different wide character, its second half will become an orphaned
        // WIDE_CONTINUATION cell. Detect this before writing (#7656).
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

        // Mark all affected cells as damaged.
        if let Some(oc) = orphan_col {
            self.grid.damage_mut().mark_cell(row, oc);
        }
        self.grid.damage_mut().mark_cell(row, col);
        self.grid.damage_mut().mark_cell(row, cont_col);

        // Advance cursor: the continuation cell consumed the column the cursor
        // was sitting on, so we need to move forward by 1. Handle wrap state.
        if self.grid.pending_wrap() {
            // Cursor was already at the last column (pending_wrap set after
            // writing the base char at col). The continuation cell is at col+1
            // which is past end-of-line — we keep pending_wrap set.
            // Nothing more to do.
        } else if self.modes.auto_wrap {
            self.grid.advance_cursor_wrap();
        } else {
            self.grid.advance_cursor_no_wrap();
        }
    }

    /// Narrow the previous cell from 2-cell to 1-cell when VS15 (U+FE0E) follows
    /// a wide emoji character.
    ///
    /// VS15 is the text presentation selector — the inverse of VS16. When it
    /// follows a wide emoji (width 2), this function:
    /// 1. Checks if the previous cell has the WIDE flag
    /// 2. Clears the WIDE flag on the base cell
    /// 3. Sets the continuation cell (spacer) to EMPTY
    /// 4. Does NOT change the cursor position (VS15 is width 0)
    fn narrow_previous_cell_for_vs15(&mut self) {
        let Some((row, col)) = self.previous_effective_cell() else {
            return;
        };

        // Only narrow if the previous cell is currently wide.
        let Some(cell) = self.grid.cell(row, col) else {
            return;
        };
        if !cell.is_wide() {
            return;
        }

        // Snapshot the base cell's raw data so we can reconstruct without WIDE.
        let char_data = cell.char_data();
        let colors = cell.colors();
        let base_flags = cell.flags();

        // The continuation cell is always at col + 1.
        let cont_col = col + 1;
        if cont_col >= self.grid.effective_cols_for_row(row) {
            return;
        }

        // Rebuild the base cell without the WIDE flag.
        let narrow_base = crate::grid::Cell::from_raw_parts(
            char_data,
            colors,
            base_flags.difference(CellFlags::WIDE),
        );

        if let Some(row_data) = self.grid.row_mut(row) {
            row_data.set(col, narrow_base);
            row_data.set(cont_col, crate::grid::Cell::EMPTY);
        }

        // Mark affected cells as damaged.
        self.grid.damage_mut().mark_cell(row, col);
        self.grid.damage_mut().mark_cell(row, cont_col);

        // Cursor position does NOT change — VS15 is a zero-width selector.
    }

    /// Try to combine a skin tone modifier with the previous emoji cell.
    ///
    /// Emoji skin tone modifiers (U+1F3FB-U+1F3FF) should attach to the
    /// preceding emoji base as combining characters, not render as separate
    /// 2-cell wide characters. Returns `true` if the modifier was combined,
    /// `false` if it should fall through to normal rendering.
    fn try_combine_skin_tone_modifier(&mut self, modifier: char) -> bool {
        let Some((row, col)) = self.previous_effective_cell() else {
            return false;
        };

        // Check if the previous cell contains an emoji base.
        let Some(cell) = self.grid.cell(row, col) else {
            return false;
        };

        // The previous cell must be wide (emoji are width 2) to accept a modifier.
        if !cell.is_wide() {
            return false;
        }

        // Resolve the base character to verify it's an emoji modifier base.
        let base_char = if cell.is_complex() {
            self.grid.resolved_char(row, col).unwrap_or('\u{FFFD}')
        } else {
            cell.char()
        };

        if !aterm_grapheme::is_emoji_modifier_base(base_char) {
            return false;
        }

        // Combine the skin tone modifier as a combining character on the base.
        self.add_combining_to_previous_cell(modifier);
        true
    }

    /// Combine the SECOND regional indicator of a flag pair into the first RI's
    /// cell, so `🇺🇸` is one 2-cell grapheme (the colour font has a bitmap for
    /// the pair, not single RIs). Returns `false` unless the previous cell is a
    /// LONE regional indicator — one still waiting for its partner — so RIs pair
    /// left to right and a third RI starts a fresh pair (Unicode GB12/GB13).
    fn try_combine_regional_indicator(&mut self, ri: char) -> bool {
        let Some((row, col)) = self.previous_effective_cell() else {
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
        // Already a complete pair (its combining store holds an RI)? Then this
        // RI begins a NEW pair in its own cell rather than extending the old one.
        let already_paired = self
            .grid
            .cell_extra(row, col)
            .is_some_and(|e| e.combining().iter().copied().any(is_regional_indicator));
        if already_paired {
            return false;
        }
        self.add_combining_to_previous_cell(ri);
        true
    }
}

/// Check if a character is a Unicode REGIONAL INDICATOR (U+1F1E6–U+1F1FF).
///
/// A pair of these forms one flag emoji (`🇺` + `🇸` = `🇺🇸`); the write path
/// folds the second into the first cell so the pair is one grapheme. Mirrors
/// `aterm_grapheme::is_regional_indicator` (test-only-exported there) as a small
/// local check to avoid widening that crate's public API.
#[inline]
fn is_regional_indicator(c: char) -> bool {
    (0x1F1E6..=0x1F1FF).contains(&(c as u32))
}

/// Check if a character is an emoji skin tone modifier (Fitzpatrick scale).
///
/// U+1F3FB (Type-1-2) through U+1F3FF (Type-6) are the five skin tone
/// modifiers defined in Unicode Technical Standard #51.
#[inline]
fn is_emoji_skin_tone_modifier(c: char) -> bool {
    (0x1F3FB..=0x1F3FF).contains(&(c as u32))
}

/// Check if a character is eligible for VS16 emoji presentation widening.
///
/// Delegates to [`aterm_grapheme::is_vs16_emoji_capable`] — the SINGLE source of
/// the eligibility set, shared with the scrollback materialization/reflow paths
/// in `aterm-grid` so a scrolled-back `❤️` keeps exactly the width the live
/// write path gave it (the table used to live here; it moved for that sharing).
#[inline]
pub(crate) fn is_vs16_emoji_capable(c: char) -> bool {
    aterm_grapheme::is_vs16_emoji_capable(c)
}

#[cfg(test)]
mod tests {
    use super::char_width;

    /// Exhaustive parity guard for the CJK fast-path block U+3000..U+A000.
    ///
    /// The hand-rolled `char_width` fast-path MUST agree with the authoritative
    /// `aterm_grapheme` width table for EVERY codepoint in this block, in BOTH
    /// ambiguous-width modes. The print path (`write_char` / `write_unicode_bulk`)
    /// places cells and advances the cursor from this width, while the
    /// reflow/materialize/fill paths recompute width from the same table — any
    /// divergence shifts content by one column on scroll/resize.
    ///
    /// Regression for the Yijing-hexagram width-1 bug (U+4DC0-U+4DFF were wrongly
    /// narrow) and the blanket-width-2 misclassification of U+303F, the ambiguous
    /// run U+3248-U+324F, the Hangul tone marks U+302E/U+302F, and the
    /// unassigned-narrow gaps in the block.
    #[test]
    fn fast_path_width_matches_table_cjk_block() {
        for cp in 0x3000u32..0xA000 {
            let c = char::from_u32(cp).expect("U+3000..U+A000 are all BMP scalar values");
            assert_eq!(
                char_width(c, false),
                aterm_grapheme::char_width(c),
                "non-CJK (ambiguous=narrow) width mismatch at U+{cp:04X}"
            );
            assert_eq!(
                char_width(c, true),
                aterm_grapheme::char_width_cjk(c),
                "CJK (ambiguous=wide) width mismatch at U+{cp:04X}"
            );
        }
    }

    /// Pin the specific divergences called out in the bug report so a future
    /// edit to the fast-path can't silently re-break them even if the table
    /// itself were to change.
    #[test]
    fn known_cjk_width_regressions() {
        // U+4DC0-U+4DFF Yijing Hexagram Symbols: East Asian Wide = 2 (was 1).
        assert_eq!(char_width('\u{4DC0}', false), 2);
        assert_eq!(char_width('\u{4DC0}', true), 2);
        assert_eq!(char_width('\u{4DFF}', false), 2);

        // U+303F IDEOGRAPHIC HALF FILL SPACE: EAW Narrow = 1 (was wrongly 2).
        assert_eq!(char_width('\u{303F}', false), 1);
        assert_eq!(char_width('\u{303F}', true), 1);

        // U+3248-U+324F: East Asian Ambiguous — 1 in default mode, 2 in CJK mode.
        assert_eq!(char_width('\u{3248}', false), 1);
        assert_eq!(char_width('\u{3248}', true), 2);
        assert_eq!(char_width('\u{324F}', false), 1);
        assert_eq!(char_width('\u{324F}', true), 2);

        // U+302E/U+302F Hangul tone marks: East Asian Wide = 2 (were wrongly 0).
        assert_eq!(char_width('\u{302E}', false), 2);
        assert_eq!(char_width('\u{302F}', false), 2);

        // Genuine zero-width combining marks stay 0.
        assert_eq!(char_width('\u{302A}', false), 0);
        assert_eq!(char_width('\u{302D}', false), 0);
        assert_eq!(char_width('\u{3099}', false), 0);
        assert_eq!(char_width('\u{309A}', false), 0);

        // An unassigned-narrow gap and a plain CJK ideograph for good measure.
        assert_eq!(char_width('\u{3040}', false), 1); // unassigned -> narrow
        assert_eq!(char_width('\u{4E00}', false), 2); // CJK ideograph
        assert_eq!(char_width('\u{9FFF}', false), 2); // end of ideograph block
    }
}
