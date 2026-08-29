// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Full-fidelity scrollback materialization (#4216).
//!
//! Converts scrollback [`Line`]s into [`MaterializedRow`]s that bundle
//! cells with supplementary [`CellExtra`] data for hyperlinks, complex
//! characters, and RGB colors.  The bridge renderer queries these extras
//! the same way it queries visible-area `CellExtras`.

use std::sync::Arc;

use aterm_hash::FxHashMap;
use aterm_scrollback::{CellAttrs, HyperlinkSpan, ImageSpan, Line, UnderlineColorSpan};

use super::scroll_convert::{
    RowToLineCursorState, ScrolledRowExtras, coalesce_underline_spans, is_spacer, next_combining,
    next_complex_char, resolve_cell_color,
};
use crate::CellExtra;
use crate::CellFlags;
use crate::ImageRef;
use crate::PackedColor;
use crate::Row;

/// A scrollback row materialized for rendering with full fidelity.
///
/// Bundles cells with supplementary [`CellExtra`] data for columns that need
/// hyperlinks, complex characters, or RGB colors.  This allows scrollback
/// cells to be rendered identically to visible-area cells.
///
/// The bridge's `RenderableCellIterator` calls [`get_extra`](Self::get_extra)
/// for scrollback cells using the same code path it uses for visible cells.
// `PartialEq` is what the viewport row memo's debug net compares against (see
// `viewport_row_cache`): every cached HIT is re-materialized and checked
// field-for-field in debug builds, because a stale hit is a wrong glyph on
// screen. Additive: `Cell` and `CellExtra` are both already `PartialEq`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterializedRow {
    /// The cells for this row (one per column).
    pub cells: Vec<super::Cell>,
    /// Sparse extras for columns that have hyperlinks, complex chars, or RGB.
    extras: FxHashMap<u16, CellExtra>,
    /// Soft-wrap flag carried over from the source [`Line`] (`Line::is_wrapped`),
    /// so a materialized history row reports the same wrap continuation state as a
    /// live `Row` — backing the tier-aware `row_is_wrapped` accessor.
    wrapped: bool,
}

impl MaterializedRow {
    /// Look up extras for a column (mirrors `CellExtras::get`).
    #[must_use]
    #[inline]
    pub fn get_extra(&self, col: u16) -> Option<&CellExtra> {
        self.extras.get(&col)
    }

    /// Iterate this row's populated extras as `(col, extra)`.
    ///
    /// The sparse counterpart to probing [`get_extra`](Self::get_extra) for
    /// every column: a per-frame reader that wants the few columns carrying
    /// something (the inline-image fill) pays for the entries that exist rather
    /// than `cols` hash probes per scrolled-back row per frame. Order is the
    /// map's, i.e. arbitrary — callers that need columns in order sort, exactly
    /// as the live `CellExtras::iter` consumers do.
    pub fn extras_iter(&self) -> impl Iterator<Item = (u16, &CellExtra)> + '_ {
        self.extras.iter().map(|(&col, extra)| (col, extra))
    }

    /// Whether this row is a soft-wrap continuation of the previous line
    /// (mirrors `Row::is_wrapped` for materialized history rows).
    #[must_use]
    #[inline]
    pub(crate) fn is_wrapped(&self) -> bool {
        self.wrapped
    }

    /// Whether this row has no occupied columns.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Compute the effective row length (last occupied column + 1).
    ///
    /// Matches `Row::len` for visible rows, which is the write high-water mark:
    /// a cell counts if it is not a DEFAULT blank — i.e. a non-space glyph, a wide
    /// cell/continuation, non-default color or style flags (`!Cell::is_empty`), OR
    /// it carries extras (complex char, combining marks, RGB overflow, hyperlink,
    /// underline color). Colors/flags live inline on the materialized cell, so a
    /// trailing coloured blank (a status bar / `\e[K`-filled row) is NOT clipped —
    /// before this it vanished on scrollback because the predicate tested only the
    /// glyph, while the live `Row` still reserved those columns.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)] // cells.len() bounded by terminal width (≤ u16::MAX)
    pub(crate) fn len(&self) -> u16 {
        self.cells
            .iter()
            .enumerate()
            .rposition(|(idx, cell)| {
                let col = idx as u16;
                !cell.is_empty()
                    || self.get_extra(col).is_some_and(|extra| {
                        extra.complex_char().is_some()
                            || !extra.combining().is_empty()
                            || extra.fg_rgb().is_some()
                            || extra.bg_rgb().is_some()
                            || extra.hyperlink().is_some()
                            || extra.underline_color().is_some()
                            || extra.is_underline_color_indexed()
                    })
            })
            .and_then(|idx| u16::try_from(idx + 1).ok())
            .unwrap_or(0)
    }
}

/// Materialize a scrollback [`Line`] into a [`MaterializedRow`] with full
/// fidelity.
///
/// Preserves all data by populating [`CellExtra`] entries for columns
/// that need them (hyperlinks, complex chars, RGB colors).
///
/// ## What's restored
///
/// - **Hyperlinks** from `Line::hyperlinks()` → `CellExtra::set_hyperlink`
/// - **Non-BMP characters** (emoji, math symbols) → `CellExtra::set_complex_char`
/// - **ZWJ sequences** (family emoji, flag emoji) → `CellExtra::set_complex_char`
/// - **Combining marks** (diacritics) → `CellExtra::push_combining`
/// - **RGB foreground** (0x01_RRGGBB in CellAttrs) → `CellExtra::set_fg_rgb`
/// - **RGB background** → `CellExtra::set_bg_rgb`
#[must_use]
pub fn materialize_from_line(line: &Line, cols: u16) -> MaterializedRow {
    use crate::{Cell, CellFlags};

    let mut row = MaterializedRow {
        cells: vec![Cell::default(); cols as usize],
        extras: FxHashMap::default(),
        // Preserve the source line's soft-wrap flag so the materialized history
        // row reports the same continuation state a live `Row` would.
        wrapped: line.is_wrapped(),
    };

    let Some(text) = line.as_str() else {
        return row;
    };

    // E6a: `unit_char_start` is monotone across this walk, so a run-cursor
    // reads the RLE attrs in O(runs) TOTAL instead of `get_attr`'s
    // rescan-from-start per cell (which made the walk O(cols × runs) — the
    // accidental attr term in the scrolled-frame cost).
    let mut attr_cursor = line.attr_cursor();
    let mut byte_idx: usize = 0;
    let mut char_idx: usize = 0;
    let mut col: u16 = 0;

    while byte_idx < text.len() && col < cols {
        // byte_idx is always at a char boundary (advanced by char_indices).
        let c = text[byte_idx..]
            .chars()
            .next()
            .expect("invariant: byte_idx < text.len()");

        // Skip orphan zero-width characters at the start of text.
        let base_width = aterm_grapheme::char_width(c);
        if base_width == 0 {
            byte_idx += c.len_utf8();
            char_idx += 1;
            continue;
        }

        let unit_byte_start = byte_idx;
        let unit_char_start = char_idx;
        let unit = advance_grapheme_unit_wide(text, &mut byte_idx);
        let chars_consumed = unit.chars;
        char_idx += chars_consumed;
        let unit_str = &text[unit_byte_start..byte_idx];

        let attrs = attr_cursor.attr_at(unit_char_start);
        let flags = CellFlags::from_bits(attrs.flags);
        // Effective width, with the live writer's row-edge exception: a VS16
        // widening FAILS at the last column (no room for the continuation
        // spacer), so the live cell stayed narrow there — demote to match
        // instead of dropping it as an unfittable wide glyph.
        let is_wide = stored_unit_is_wide(unit, attrs) && !(unit.vs16_widened && col + 1 >= cols);
        let is_complex = chars_consumed > 1 || c as u32 > super::Cell::MAX_DIRECT_CODEPOINT;

        let fg = PackedColor(attrs.fg);
        let bg = PackedColor(attrs.bg);

        let prev_col = col;
        col = place_cell(
            &mut row, col, cols, c, unit_str, fg, bg, flags, is_wide, is_complex,
        );

        // Store RGB colors in extras for the cell we just placed.
        // Skip if place_cell didn't advance col (wide char dropped at last column).
        if col > prev_col {
            store_rgb_extras(&mut row.extras, prev_col, cols, &attrs);
        }

        // No column progress ⇒ a wide char could not fit at the last column, so the
        // row is full. Stop instead of scanning the rest of a possibly-oversized
        // (injected/restored) line unit-by-unit for cells that can never be placed
        // — that scan is O(line length) work under the lock (a UI-freeze DoS). A
        // legitimate stored physical row never starts a wide char it cannot fit, so
        // this never truncates real content.
        if col == prev_col {
            break;
        }
    }

    // Restore hyperlinks from Line into extras.
    restore_hyperlinks(&mut row.extras, line, cols);

    // Restore SGR 58 underline colours from Line into extras.
    restore_underline_colors(&mut row.extras, line, cols);

    // Restore inline images from Line into extras.
    restore_images(&mut row.extras, line, cols);

    row
}

/// Materialize a RING-tier history row DIRECTLY from its stored `Row` +
/// `ScrolledRowExtras`, skipping the `Line` round trip — or `None` when this
/// path cannot PROVE it would produce the same row, in which case the caller
/// falls back to the round trip (SCR-2).
///
/// ## What it deletes
///
/// A ring-tier read used to go Row -> `Line` -> cells: `row_to_line_with_stored_extras`
/// builds a `String` of the row's text plus an `Rle<CellAttrs>` and clones the
/// hyperlink span vector (an O(cols) pass and ~4 allocations), and
/// [`materialize_from_line`] then parses that text straight back into cells with
/// a grapheme + `char_width` + attr-cursor walk (a second O(cols) pass). Both
/// ends of that trip are in RAM as real `Cell`s the whole time: it is a
/// representation mismatch, not a cache miss, and it is paid for the newest
/// ~10_000 lines — i.e. essentially all interactive scrollback depth. This path
/// keeps the second half's PLACEMENT (it calls the very same
/// [`place_cell`]/[`store_rgb_extras`]/restore helpers) and deletes the first
/// half entirely, along with the text re-parse for every ordinary cell.
///
/// ## Why it is allowed to give up
///
/// The round trip is NOT an identity function — it NORMALIZES. Combining marks
/// fold into the cell's complex string; a "complex" cell holding a single BMP
/// scalar demotes to an inline char; a zero-width base is DROPPED (shifting
/// every later column); a cluster over
/// [`MAX_GRAPHEME_UNIT_BYTES`] is clipped; a wide unit that cannot fit ends the
/// row. Every consumer of a materialized history row is written against the
/// NORMALIZED shape (see `CellDataView::marks`, which reconstructs `(base,
/// marks)` out of `complex_char.chars().skip(1)`), so "more faithful" here would
/// be WRONG, not better.
///
/// So this path reproduces the normalization for the cases it can prove, and
/// BAILS on the rest:
///
/// * the emitted unit does not consume the cell's whole text in ONE grapheme
///   unit (the round trip would have split it across extra columns),
/// * the unit's base scalar is zero-width (the round trip drops it and shifts),
/// * the placed width disagrees with the source row's physical layout — checked
///   as a running invariant, `output column == physical column`, plus a final
///   total, so a divergence anywhere aborts the whole row,
/// * placement made no progress (a wide unit at the last column: the round trip
///   breaks out of its loop there).
///
/// A bail costs one wasted `vec![Cell; cols]` and then does exactly what the
/// code did before, so the fast path can only ever be a speed decision, never a
/// correctness one. The parity test (`aterm-core`
/// `tests/scrollback_ring_materialize_parity.rs`) drives a corpus of ASCII, CJK,
/// emoji/ZWJ/VS16, combining marks, truecolor, OSC 8 and SGR 58 through the real
/// parser and asserts cell-for-cell and extra-for-extra equality with the round
/// trip — AND, two-sided, that the fast path actually fired.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one linear transcription of the Line round trip's per-cell derivation; \
              splitting it would put the two halves of a parity argument in two places"
)]
pub(in crate::grid) fn materialize_from_row_extras(
    row: &Row,
    extras: Option<&ScrolledRowExtras>,
    cols: u16,
) -> Option<MaterializedRow> {
    use crate::Cell;

    // `None` means "this ring row had no overflow data at all", which is what
    // the reader's `unwrap_or(&default)` already substitutes.
    let no_extras = ScrolledRowExtras::default();
    let extras = extras.unwrap_or(&no_extras);

    let len = row.len() as usize;
    if len > cols as usize {
        // A row wider than the requested materialization width: the round trip
        // clips it through the text walk's `col < cols` bound, which this path
        // does not model. Rare-to-impossible (both come from the same grid), and
        // the fallback handles it exactly.
        return None;
    }

    let mut out = MaterializedRow {
        cells: vec![Cell::default(); cols as usize],
        extras: FxHashMap::default(),
        // Same source as the round trip's: `row_to_line_*` copies the row's flag
        // onto the Line, and `materialize_from_line` copies it back off.
        wrapped: row.is_wrapped(),
    };
    if len == 0 {
        // The round trip builds an EMPTY Line for a zero-length row — no text,
        // no hyperlinks, no underline spans — so every cell stays default. It
        // does NOT drop the row's inline images: `place_image` writes no glyph,
        // so a picture's own rows are exactly the zero-length ones, and the
        // round trip carries their spans over the top of an empty Line. Restore
        // them here or this path diverges from it on every image row.
        restore_image_spans(&mut out.extras, &extras.images, cols);
        #[cfg(any(test, feature = "testing"))]
        super::count_ring_fast_materialize();
        return Some(out);
    }

    let mut cursors = RowToLineCursorState::default();
    let mut col: u16 = 0;
    let cells = &row.as_slice()[..len];
    // One reusable buffer per row for the rare cells that need a cluster string;
    // the common cell never touches it (see the closed-form branch below).
    let mut unit_buf = String::new();

    for (physical_col, cell) in cells.iter().enumerate() {
        // The serializer OMITS spacers, and the parser RE-CREATES them beside
        // their wide main cell. Skip exactly what it skips.
        if is_spacer(cells, physical_col) {
            continue;
        }
        let col_u16 = u16::try_from(physical_col).ok()?;
        // THE COLUMN INVARIANT. The round trip's output column tracks the
        // source's physical column only while every unit's placed width matches
        // the source cell's footprint. Checking it here is what makes every
        // width question above safe: the first disagreement aborts the row.
        if col != col_u16 {
            return None;
        }

        // --- the unit's text, exactly as the serializer would have written it --
        let plain = if cell.is_complex() {
            None
        } else {
            // NUL (empty cell) -> space, matching `push_cell_text`.
            let ch = cell.char();
            Some(if ch == '\0' { ' ' } else { ch })
        };
        let stored = if cell.is_complex() {
            next_complex_char(extras, &mut cursors, col_u16)
        } else {
            None
        };
        let marks = next_combining(extras, &mut cursors, col_u16);

        let mut char_buf = [0u8; 4];
        let base: char;
        let unit_str: &str;
        let unit: GraphemeUnit;
        if let (Some(ch), None) = (plain, marks) {
            // THE COMMON CELL: one non-complex scalar, no marks. The serializer
            // pushes exactly this char and `advance_grapheme_unit_wide` consumes
            // exactly it with nothing to join, so the unit is closed-form and
            // neither a string build nor a re-parse is needed. `char_data` is a
            // u16, so such a char can never exceed `MAX_DIRECT_CODEPOINT`.
            base = ch;
            unit_str = ch.encode_utf8(&mut char_buf);
            unit = GraphemeUnit {
                chars: 1,
                wide: aterm_grapheme::char_width(ch) >= 2,
                vs16_widened: false,
                force_narrow: false,
            };
        } else {
            unit_buf.clear();
            match (plain, stored) {
                (Some(ch), _) => unit_buf.push(ch),
                (None, Some(value)) => unit_buf.push_str(value),
                // A complex cell whose stored string is missing: the serializer
                // writes U+FFFD and one attr, so the cell materializes PLAIN.
                (None, None) => unit_buf.push('\u{FFFD}'),
            }
            if let Some(marks) = marks {
                unit_buf.extend(marks.iter().copied());
            }
            base = unit_buf.chars().next()?;
            let mut scanned = 0usize;
            unit = advance_grapheme_unit_wide(&unit_buf, &mut scanned);
            if scanned != unit_buf.len() {
                // The scan stopped early: the round trip would have emitted a
                // SECOND unit (another column) for the tail, or clipped the
                // cluster at MAX_GRAPHEME_UNIT_BYTES. Give up on the row.
                return None;
            }
            unit_str = &unit_buf;
        }
        if aterm_grapheme::char_width(base) == 0 {
            // The text walk SKIPS a zero-width base and shifts every later
            // column left. Not modelled here.
            return None;
        }

        // --- colours + flags, exactly as the serializer would have stored them -
        let fg_raw = resolve_cell_color(
            cell.fg_needs_overflow() || cell.uses_style_id(),
            cell.fg_color().map_or(PackedColor::DEFAULT_FG.0, |c| c.0),
            &extras.rgb_fg,
            &mut cursors.rgb_fg_idx,
            col_u16,
            PackedColor::DEFAULT_FG.0,
        );
        let bg_raw = resolve_cell_color(
            cell.bg_needs_overflow() || cell.uses_style_id(),
            cell.bg_color().map_or(PackedColor::DEFAULT_BG.0, |c| c.0),
            &extras.rgb_bg,
            &mut cursors.rgb_bg_idx,
            col_u16,
            PackedColor::DEFAULT_BG.0,
        );
        let attrs = CellAttrs::from_raw(fg_raw, bg_raw, cell.flags().bits());
        let flags = CellFlags::from_bits(attrs.flags);
        let is_wide = stored_unit_is_wide(unit, attrs) && !(unit.vs16_widened && col + 1 >= cols);
        let is_complex = unit.chars > 1 || base as u32 > Cell::MAX_DIRECT_CODEPOINT;

        // --- placement: the SAME functions the round trip's parser calls -------
        let prev_col = col;
        col = place_cell(
            &mut out,
            col,
            cols,
            base,
            unit_str,
            PackedColor(fg_raw),
            PackedColor(bg_raw),
            flags,
            is_wide,
            is_complex,
        );
        if col == prev_col {
            // No progress: a wide unit that cannot fit at the last column. The
            // text walk BREAKS there, dropping the rest of the row.
            return None;
        }
        store_rgb_extras(&mut out.extras, prev_col, cols, &attrs);
    }

    if usize::from(col) != len {
        // The tail placed wider (or narrower) than the source row: the running
        // invariant above cannot see a divergence on the LAST cell, so the
        // total is checked here.
        return None;
    }

    // Both restores are the round trip's own, reading the stored spans instead
    // of the copies the `Line` carried.
    restore_hyperlink_spans(&mut out.extras, &extras.hyperlinks, cols);
    if !extras.underline_colors.is_empty() {
        restore_underline_color_spans(
            &mut out.extras,
            &coalesce_underline_spans(&extras.underline_colors),
            cols,
        );
    }
    restore_image_spans(&mut out.extras, &extras.images, cols);

    #[cfg(any(test, feature = "testing"))]
    super::count_ring_fast_materialize();
    Some(out)
}

/// Absolute byte ceiling for a single grapheme unit materialized from a stored
/// `Line` ([`advance_grapheme_unit`]).
///
/// A legitimately-written cell holds a base char plus at most
/// [`Extra::MAX_COMBINING`](crate::extra::Extra::MAX_COMBINING) (16) marks — well
/// under a few hundred bytes. This ceiling only ever clips a pathological injected
/// cluster (a crafted checkpoint / scrollback `Line` with an endless ZWJ chain),
/// bounding one cell's `complex_char` allocation and therefore a whole row's
/// materialized text to `MAX_GRID_COLS` * this (~1 MiB).
const MAX_GRAPHEME_UNIT_BYTES: usize = 256;

/// Advance `byte_idx` past the current grapheme unit (one cell's worth of
/// characters) in `text`.
///
/// Returns the number of characters consumed, which callers use to maintain
/// a parallel character index for `Line::get_attr`.
///
/// Consumes the base character plus any following zero-width chars
/// (combining marks, variation selectors) and ZWJ-joined characters.
/// Callers use `&text[start..*byte_idx]` to access the consumed `&str`
/// slice without heap allocation (#5949).
///
/// Used by both `materialize_from_line` and `fill_row_from_line` to ensure
/// consistent grapheme handling when recovering content from scrollback.
///
/// A Fitzpatrick emoji skin-tone modifier (U+1F3FB..=U+1F3FF). Mirrors the
/// test-gated `aterm_grapheme::is_skin_tone_modifier` and the live writer's range.
#[inline]
const fn is_skin_tone_modifier(c: char) -> bool {
    matches!(c, '\u{1F3FB}'..='\u{1F3FF}')
}

/// A Unicode regional indicator (U+1F1E6..=U+1F1FF); a flag is a pair.
#[inline]
const fn is_regional_indicator(c: char) -> bool {
    matches!(c, '\u{1F1E6}'..='\u{1F1FF}')
}

/// Test-compatibility wrapper over [`advance_grapheme_unit_wide`] returning only
/// the consumed char count — the historical #5951-verified surface. Production
/// call sites (materialize / fill / reflow) all need the effective width and use
/// the `_wide` variant directly, so this is test-gated to stay dead-code-clean.
#[cfg(test)]
pub(crate) fn advance_grapheme_unit(text: &str, byte_idx: &mut usize) -> usize {
    advance_grapheme_unit_wide(text, byte_idx).chars
}

/// One scanned grapheme unit: the consumed char count plus its EFFECTIVE cell
/// width (see [`advance_grapheme_unit_wide`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct GraphemeUnit {
    /// Characters consumed (callers advance their parallel char index by this).
    pub(crate) chars: usize,
    /// The unit's effective width is 2 cells (after VS16/VS15 replay).
    pub(crate) wide: bool,
    /// `wide` is true ONLY because VS16 widened a narrow base (no intrinsically
    /// wide scalar). The live `widen_previous_cell_for_vs16` FAILS when there is
    /// no room for the continuation spacer (base at the last column), so
    /// restore paths demote such a unit back to narrow at the row edge —
    /// matching the live cell instead of dropping it as an unfittable wide.
    pub(crate) vs16_widened: bool,
    /// The unit's final width was explicitly narrowed by VS15. This overrides a
    /// stale/authored `WIDE` attribute: presentation selectors are part of the
    /// stored text and must keep their live write-path semantics.
    force_narrow: bool,
}

/// Resolve a Line-backed unit's write-time cell geometry.
///
/// A live grid stores a wide main cell with [`CellFlags::WIDE`], including an
/// East-Asian-Ambiguous scalar printed while the terminal's CJK-width policy was
/// active. Row-to-Line conversion preserves that bit while omitting the spacer.
/// Recomputing solely from Unicode's default (narrow-ambiguous) table therefore
/// collapses the cell after it enters scrollback. Prefer either source of wide
/// geometry, except that an explicit VS15 in the text remains authoritative.
#[inline]
pub(crate) fn stored_unit_is_wide(unit: GraphemeUnit, attrs: CellAttrs) -> bool {
    !unit.force_narrow && (unit.wide || CellFlags::from_bits(attrs.flags).contains(CellFlags::WIDE))
}

/// [`advance_grapheme_unit`] plus the unit's EFFECTIVE cell width: returns a
/// [`GraphemeUnit`] whose `wide` replays the live writer's
/// presentation-selector width transitions instead of reading only the base
/// scalar's `char_width`:
///
/// * **VS16** (U+FE0F) WIDENS a narrow, emoji-capable base to 2 cells
///   (mirroring `widen_previous_cell_for_vs16`, which gates on `!is_wide()` +
///   `is_vs16_emoji_capable(base)`), so a scrolled-back `❤️` keeps the two
///   columns the live grid gave it.
/// * **VS15** (U+FE0E) NARROWS a currently-wide unit back to 1 cell
///   (mirroring `narrow_previous_cell_for_vs15`, which gates on `is_wide()`),
///   so `⌚︎` stays one column.
///
/// The width is tracked THROUGH the scan because the live writer applies each
/// selector to the cell's width *at that moment* (`⌚` + VS15 + VS16 re-widens),
/// and because the skin-tone fold below gates on it.
pub(crate) fn advance_grapheme_unit_wide(text: &str, byte_idx: &mut usize) -> GraphemeUnit {
    let remaining = &text[*byte_idx..];
    let mut iter = remaining.char_indices();
    let Some((_, c)) = iter.next() else {
        return GraphemeUnit {
            chars: 0,
            wide: false,
            vs16_widened: false,
            force_narrow: false,
        };
    };

    let mut end = c.len_utf8();
    let mut char_count: usize = 1;
    let mut last_was_zwj = c == '\u{200D}';
    // The unit's EFFECTIVE width so far (see the doc above): starts from the
    // base scalar, then follows the VS16/VS15 transitions like the live cell.
    let mut wide = aterm_grapheme::char_width(c) >= 2;
    // Whether the CURRENT wide state came from VS16 widening a narrow base
    // (the live widen can fail at the row edge — see [`GraphemeUnit`]).
    let mut vs16_widened = false;
    // True only while the latest width-selecting presentation selector is VS15.
    // A later effective VS16 clears it again, matching the live writer.
    let mut force_narrow = false;
    // The most-recently-absorbed scalar, for the flag-pair join test below (a
    // second regional indicator folds onto the immediately-preceding one). The
    // skin-tone test instead uses the unit BASE `c`, so an intervening zero-width
    // scalar (VS16 / combining) does not defeat the fold.
    let mut prev_c = c;
    // A flag is exactly TWO regional indicators; a third starts a new unit.
    let mut absorbed_ri = false;

    for (offset, next_c) in iter {
        // Bound the unit length. The live write path caps a cell at a base char plus
        // `Extra::MAX_COMBINING` marks (tens of bytes); a legitimate round-tripped
        // cluster is far under MAX_GRAPHEME_UNIT_BYTES. But `text` here can come from
        // a crafted checkpoint / injected scrollback `Line` (deserialize has no
        // per-line content cap), so an endless ZWJ/combining chain would otherwise be
        // consumed into ONE unit and allocated whole into a single `complex_char`
        // cell (`Arc::from(unit_str)`) — a memory-amplification DoS also hit by
        // reflow and render. Stop growing the unit at the ceiling; the excess chars
        // form later units (zero-width ones are skipped, and any overflow past the
        // grid width is dropped by the `col < cols` bound).
        if end >= MAX_GRAPHEME_UNIT_BYTES {
            break;
        }
        let next_width = aterm_grapheme::char_width(next_c);

        if next_c == '\u{200D}' {
            last_was_zwj = true;
        } else if next_c == '\u{FE0F}' {
            // VS16 joins the unit (zero-width) AND widens a narrow emoji-capable
            // base, exactly like the live `widen_previous_cell_for_vs16`.
            if !wide && aterm_grapheme::is_vs16_emoji_capable(c) {
                wide = true;
                vs16_widened = true;
                force_narrow = false;
            }
            last_was_zwj = false;
        } else if next_c == '\u{FE0E}' {
            // VS15 joins the unit AND narrows a currently-wide cell, exactly
            // like the live `narrow_previous_cell_for_vs15`.
            wide = false;
            vs16_widened = false;
            force_narrow = true;
            last_was_zwj = false;
        } else if next_width == 0 || last_was_zwj {
            // Zero-width chars (combining marks, other selectors)
            // or the visible char after a ZWJ — both join the current unit.
            last_was_zwj = false;
        } else if is_skin_tone_modifier(next_c) && aterm_grapheme::is_emoji_modifier_base(c) && wide
        {
            // A Fitzpatrick skin-tone modifier folds onto the unit's emoji BASE,
            // matching the live writer's `try_combine_skin_tone_modifier`, which
            // gates on BOTH the previous cell being WIDE and
            // `is_emoji_modifier_base(base)`. The width gate uses the EFFECTIVE
            // width, so `☝️🏽` (base + widening VS16 + modifier) still folds —
            // the live cell was VS16-widened when the modifier arrived — while a
            // text-presentation `☝🏽` stays SPLIT (live: the combine fails on the
            // narrow cell and the modifier renders as its own wide cell).
            last_was_zwj = false;
        } else if is_regional_indicator(next_c) && is_regional_indicator(prev_c) && !absorbed_ri {
            // The second regional indicator of a flag pair folds onto the first,
            // matching `try_combine_regional_indicator`; at most one pair per unit.
            absorbed_ri = true;
            last_was_zwj = false;
        } else {
            break;
        }

        end = offset + next_c.len_utf8();
        char_count += 1;
        prev_c = next_c;
    }

    *byte_idx += end;
    GraphemeUnit {
        chars: char_count,
        wide,
        vs16_widened,
        force_narrow,
    }
}

/// Place a cell (complex, wide, or normal) into the materialized row.
///
/// Accepts `unit_str` as a `&str` slice borrowed from the source text,
/// avoiding heap allocation entirely (#5949). `Arc::from(unit_str)` is
/// used directly for complex characters that need storage.
///
/// Returns the new column position after placement.
#[allow(clippy::too_many_arguments)]
fn place_cell(
    row: &mut MaterializedRow,
    col: u16,
    cols: u16,
    c: char,
    unit_str: &str,
    fg: PackedColor,
    bg: PackedColor,
    flags: crate::CellFlags,
    is_wide: bool,
    is_complex: bool,
) -> u16 {
    use crate::{Cell, CellFlags};

    let cell_flags = if is_wide {
        flags.union(CellFlags::WIDE)
    } else {
        flags.difference(CellFlags::WIDE)
    };

    if is_complex {
        let mut cell = Cell::with_style(' ', fg, bg, cell_flags);
        cell.set_overflow_index(0);

        row.extras
            .entry(col)
            .or_default()
            .set_complex_char(Some(Arc::from(unit_str)));

        if is_wide && col + 1 < cols {
            row.cells[col as usize] = cell;
            row.cells[(col + 1) as usize] =
                Cell::with_style(' ', fg, bg, CellFlags::WIDE_CONTINUATION);
            col.saturating_add(2)
        } else if !is_wide {
            row.cells[col as usize] = cell;
            col.saturating_add(1)
        } else {
            col // wide at last column — drop
        }
    } else if is_wide {
        if col + 1 < cols {
            row.cells[col as usize] = Cell::with_style(c, fg, bg, cell_flags);
            row.cells[(col + 1) as usize] =
                Cell::with_style(' ', fg, bg, CellFlags::WIDE_CONTINUATION);
            col.saturating_add(2)
        } else {
            col
        }
    } else {
        row.cells[col as usize] = Cell::with_style(c, fg, bg, cell_flags);
        col.saturating_add(1)
    }
}

/// Store RGB color data from CellAttrs into extras.
fn store_rgb_extras(
    extras: &mut FxHashMap<u16, CellExtra>,
    placed_col: u16,
    cols: u16,
    attrs: &CellAttrs,
) {
    if placed_col >= cols {
        return;
    }
    let fg_is_rgb = (attrs.fg >> 24) == 0x01;
    let bg_is_rgb = (attrs.bg >> 24) == 0x01;
    if !fg_is_rgb && !bg_is_rgb {
        return;
    }
    let extra = extras.entry(placed_col).or_default();
    if fg_is_rgb {
        let r = ((attrs.fg >> 16) & 0xFF) as u8;
        let g = ((attrs.fg >> 8) & 0xFF) as u8;
        let b = (attrs.fg & 0xFF) as u8;
        extra.set_fg_rgb(Some([r, g, b]));
    }
    if bg_is_rgb {
        let r = ((attrs.bg >> 16) & 0xFF) as u8;
        let g = ((attrs.bg >> 8) & 0xFF) as u8;
        let b = (attrs.bg & 0xFF) as u8;
        extra.set_bg_rgb(Some([r, g, b]));
    }
}

/// Restore hyperlinks from Line into extras.
///
/// Bounds total column-writes to O(cols): each of the row's `cols` cells holds at
/// most one hyperlink, so a legit (disjoint) line writes <= cols columns and never
/// hits the budget. A crafted Line with many OVERLAPPING spans (each [0, cols))
/// would otherwise make this O(spans * cols) on the scrollback render path — the
/// same DoS as `fill_row_from_line`. The budget truncates only crafted overlap.
fn restore_hyperlinks(extras: &mut FxHashMap<u16, CellExtra>, line: &Line, cols: u16) {
    if let Some(spans) = line.hyperlinks() {
        restore_hyperlink_spans(extras, spans, cols);
    }
}

/// The body of [`restore_hyperlinks`], over spans from either source (a `Line`'s
/// copy or the ring row's stored vector) — shared so the two materialization
/// paths cannot drift on the overlap budget or the end-column clamp.
fn restore_hyperlink_spans(
    extras: &mut FxHashMap<u16, CellExtra>,
    spans: &[HyperlinkSpan],
    cols: u16,
) {
    let mut budget = cols;
    'restore: for span in spans {
        for hcol in span.start_col..span.end_col.min(cols) {
            if budget == 0 {
                break 'restore;
            }
            let extra = extras.entry(hcol).or_default();
            extra.set_hyperlink(Some(span.url.clone()));
            extra.set_hyperlink_id(span.id.clone());
            budget -= 1;
        }
    }
}

/// Restore SGR 58 underline colours from Line into extras.
///
/// Mirrors [`restore_hyperlinks`]: fills each span's `[start_col, end_col)`
/// cell range with the packed colour via `set_underline_color_u32`, which
/// re-derives the RGB-vs-indexed form (`0x01`/`0x02`) so an indexed colour
/// resolves against the live palette at render time. Bounds total column-writes
/// to O(cols) against a crafted Line with many OVERLAPPING spans (the same DoS
/// guard as the hyperlink restore); a legit (disjoint) line never hits it.
fn restore_underline_colors(extras: &mut FxHashMap<u16, CellExtra>, line: &Line, cols: u16) {
    if let Some(spans) = line.underline_colors() {
        restore_underline_color_spans(extras, spans, cols);
    }
}

/// The body of [`restore_underline_colors`], over spans from either source —
/// shared for the same reason as [`restore_hyperlink_spans`].
fn restore_underline_color_spans(
    extras: &mut FxHashMap<u16, CellExtra>,
    spans: &[UnderlineColorSpan],
    cols: u16,
) {
    let mut budget = cols;
    'restore: for span in spans {
        for ucol in span.start_col..span.end_col.min(cols) {
            if budget == 0 {
                break 'restore;
            }
            extras
                .entry(ucol)
                .or_default()
                .set_underline_color_u32(Some(span.color));
            budget -= 1;
        }
    }
}

/// Restore inline images from a Line into extras.
///
/// Mirrors [`restore_hyperlinks`]: expands each span back to the per-cell
/// [`ImageRef`](crate::ImageRef)s the renderer reads, which is the form a LIVE
/// image cell has, so a scrolled-back picture takes the very same (pixel-tested)
/// draw path as one on screen. The payload `Arc` is cloned, never the raster, so
/// every restored cell of every restored row points at the ONE allocation the
/// renderer's decode cache is keyed on.
fn restore_images(extras: &mut FxHashMap<u16, CellExtra>, line: &Line, cols: u16) {
    if let Some(spans) = line.images() {
        restore_image_spans(extras, spans, cols);
    }
}

/// The body of [`restore_images`], over spans from either source — shared for
/// the same reason as [`restore_hyperlink_spans`].
///
/// Bounds total column-writes to O(cols) against a stored `Line` carrying many
/// OVERLAPPING spans (the same crafted-input guard as the hyperlink and
/// underline restores); a legit line's spans are disjoint and never hit it.
fn restore_image_spans(extras: &mut FxHashMap<u16, CellExtra>, spans: &[ImageSpan], cols: u16) {
    let mut budget = cols;
    'restore: for span in spans {
        for icol in span.start_col..span.end_col.min(cols) {
            if budget == 0 {
                break 'restore;
            }
            if let Some((cell_row, cell_col)) = span.tile_at(icol) {
                extras.entry(icol).or_default().set_image(Some(ImageRef {
                    image: Arc::clone(&span.image),
                    cell_row,
                    cell_col,
                }));
            }
            budget -= 1;
        }
    }
}
