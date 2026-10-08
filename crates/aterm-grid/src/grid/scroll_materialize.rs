// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Full-fidelity scrollback materialization.
//!
//! Converts scrollback [`Line`]s into [`MaterializedRow`]s that bundle cells
//! with supplementary [`CellExtra`] data for hyperlinks, complex characters,
//! and RGB colors, which the bridge renderer queries the same way it queries
//! visible-area `CellExtras`.

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
// `PartialEq` is used by the viewport row memo's debug check (see
// `viewport_row_cache`), which re-materializes every cached hit and compares
// it field-for-field, since a stale hit is a wrong glyph on screen.
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
    /// The sparse counterpart to probing [`get_extra`](Self::get_extra) per
    /// column, for per-frame readers wanting the few columns that carry
    /// something (the inline-image fill). Order is arbitrary; callers needing
    /// column order sort, as with `CellExtras::iter`.
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
    /// Matches `Row::len` (the write high-water mark): a cell counts if it is
    /// not a default blank (a non-space glyph, a wide cell/continuation,
    /// non-default color or style flags, i.e. `!Cell::is_empty`) or carries
    /// extras (complex char, combining marks, RGB overflow, hyperlink, underline
    /// color). A trailing coloured blank (a status bar, a `\e[K`-filled row) is
    /// therefore kept, as on the live row.
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
/// - **Combining marks** ride in their grapheme unit's complex char →
///   `CellExtra::set_complex_char`
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

    // `unit_char_start` is monotone across this walk, so a run cursor reads the
    // RLE attrs in O(runs) total instead of rescanning per cell.
    let mut attr_cursor = line.attr_cursor();
    let mut byte_idx: usize = 0;
    let mut char_idx: usize = 0;
    let mut col: u16 = 0;

    while byte_idx < text.len() && col < cols {
        // byte_idx is always at a char boundary (advanced by char_indices).
        let c = char_at(text, byte_idx);

        // Skip orphan zero-width characters at the start of text.
        if is_zero_width(c) {
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
        // widening fails at the last column (no room for the spacer), so the
        // live cell stayed narrow there; demote to match rather than drop it.
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

        // No column progress: a wide char could not fit at the last column, so
        // the row is full. Stop instead of scanning the rest of a possibly
        // oversized (injected/restored) line under the lock. A legitimate stored
        // row never starts a wide char it cannot fit, so no real content is cut.
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

    // LAST: the spacer mirror reads what the restores above just wrote, so it
    // must run after all of them.
    mirror_wide_spacer_extras(&row.cells, &mut row.extras);

    row
}

/// Materialize a ring-tier history row directly from its stored `Row` +
/// `ScrolledRowExtras`, skipping the `Line` round trip; or `None` when this
/// path cannot prove it would produce the same row, in which case the caller
/// falls back to the round trip.
///
/// The round trip (`row_to_line_with_stored_extras` building a `String`, an
/// `Rle<CellAttrs>` and cloned hyperlink spans, then [`materialize_from_line`]
/// re-parsing them) is two O(cols) passes and several allocations for data
/// already held as `Cell`s, for essentially all interactive scrollback depth.
/// This path keeps the placement (the same [`place_cell`] /
/// [`store_rgb_extras`] / restore helpers) and drops the rest.
///
/// The round trip normalizes rather than being an identity: combining marks
/// fold into the cell's complex string, a complex cell holding one BMP scalar
/// demotes to inline, a zero-width base is dropped (shifting later columns), a
/// cluster over [`MAX_GRAPHEME_UNIT_BYTES`] is clipped, and a wide unit that
/// cannot fit ends the row. Consumers are written against that normalized
/// shape (e.g. `CellDataView::marks`), so this path reproduces it where it can
/// prove it and bails when:
///
/// * the emitted unit does not consume the cell's whole text in one grapheme
///   unit,
/// * the unit's base scalar is zero-width,
/// * the placed width disagrees with the source row's layout (checked as a
///   running `output column == physical column` invariant plus a final
///   total),
/// * placement made no progress (a wide unit at the last column).
///
/// A bail costs one wasted `vec![Cell; cols]` and then takes the round trip,
/// so this is only a speed decision. `aterm-core`'s
/// `tests/scrollback_ring_materialize_parity.rs` drives ASCII, CJK,
/// emoji/ZWJ/VS16, combining marks, truecolor, OSC 8 and SGR 58 through the
/// parser and checks equality with the round trip, and that the fast path fired.
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
        // A row wider than the requested width: the round trip clips it via the
        // text walk's `col < cols` bound, which this path does not model. Both
        // come from the same grid, so this is rare; the fallback handles it.
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
        // The round trip builds an empty Line for a zero-length row, so every
        // cell stays default, but it keeps the row's inline images: an image
        // row has no glyphs and so is zero-length. Restore them here too.
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
        // Column invariant: the round trip's output column tracks the source's
        // physical column only while every unit's placed width matches the
        // source cell's footprint; the first disagreement aborts the row.
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
            // The common cell: one non-complex scalar, no marks. The serializer
            // pushes exactly this char and `advance_grapheme_unit_wide` consumes
            // exactly it, so no string build or re-parse is needed. `char_data`
            // is a u16, so it never exceeds `MAX_DIRECT_CODEPOINT`.
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
                // The scan stopped early: the round trip would emit a second
                // unit for the tail, or clip the cluster at
                // MAX_GRAPHEME_UNIT_BYTES. Give up on the row.
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
        // The tail placed wider or narrower than the source row; the running
        // invariant cannot see a divergence on the last cell, so check the total.
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
    // Same final step as the `Line` round trip, for the same reason: the ring
    // fast path must produce the byte-identical row or it is not a fast path.
    mirror_wide_spacer_extras(&out.cells, &mut out.extras);

    #[cfg(any(test, feature = "testing"))]
    super::count_ring_fast_materialize();
    Some(out)
}

/// Byte ceiling for a single grapheme unit materialized from a stored `Line`
/// ([`advance_grapheme_unit`]).
///
/// A legitimately written cell holds a base char plus at most
/// [`Extra::MAX_COMBINING`](crate::extra::Extra::MAX_COMBINING) (16) marks.
/// This only clips a pathological injected cluster (a crafted checkpoint or
/// scrollback `Line` with an endless ZWJ chain), bounding one cell's
/// `complex_char` and a row's text to `MAX_GRID_COLS` * this (~1 MiB).
const MAX_GRAPHEME_UNIT_BYTES: usize = 256;

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

/// Advance `byte_idx` past the current grapheme unit (one cell's worth of
/// characters: the base plus following zero-width chars and ZWJ-joined
/// characters) and return the number of characters consumed.
///
/// Test-only wrapper over [`advance_grapheme_unit_wide`], which production
/// callers (materialize / fill / reflow) use for the effective width.
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
    /// `wide` is true only because VS16 widened a narrow base. The live
    /// `widen_previous_cell_for_vs16` fails when there is no room for the
    /// spacer (base at the last column), so restore paths demote such a unit to
    /// narrow at the row edge, matching the live cell.
    pub(crate) vs16_widened: bool,
    /// The unit's final width was explicitly narrowed by VS15. This overrides a
    /// stale/authored `WIDE` attribute: presentation selectors are part of the
    /// stored text and must keep their live write-path semantics.
    force_narrow: bool,
}

/// Resolve a Line-backed unit's write-time cell geometry.
///
/// A live grid stores a wide main cell with [`CellFlags::WIDE`], including an
/// East-Asian-Ambiguous scalar printed under the CJK-width policy, and
/// Row-to-Line conversion keeps that bit while omitting the spacer.
/// Recomputing from Unicode's default table alone would collapse the cell in
/// scrollback, so either source of wide geometry counts, except that an
/// explicit VS15 in the text stays authoritative.
#[inline]
pub(crate) fn stored_unit_is_wide(unit: GraphemeUnit, attrs: CellAttrs) -> bool {
    !unit.force_narrow && (unit.wide || CellFlags::from_bits(attrs.flags).contains(CellFlags::WIDE))
}

/// [`advance_grapheme_unit`] plus the unit's effective cell width: `wide`
/// replays the live writer's presentation-selector width transitions rather
/// than reading only the base scalar's `char_width`:
///
/// * VS16 (U+FE0F) widens a narrow, emoji-capable base to 2 cells (as
///   `widen_previous_cell_for_vs16`, gated on `!is_wide()` +
///   `is_vs16_emoji_capable(base)`), so a scrolled-back `❤️` keeps two columns.
/// * VS15 (U+FE0E) narrows a wide unit to 1 cell (as
///   `narrow_previous_cell_for_vs15`), so `⌚︎` stays one column.
///
/// The width is tracked through the scan because the live writer applies each
/// selector to the width at that moment (`⌚` + VS15 + VS16 re-widens), and the
/// skin-tone fold below gates on it.
pub(crate) fn advance_grapheme_unit_wide(text: &str, byte_idx: &mut usize) -> GraphemeUnit {
    // Printable-ASCII fast path: a printable ASCII base (width 1, not a ZWJ,
    // not emoji-capable) followed by the end or another printable ASCII char
    // (width 1, not a selector, ZWJ, modifier or regional indicator) is always
    // a one-char narrow unit; the scan below would break at that second char.
    // Every joining char is non-ASCII or a zero-width ASCII control, so the
    // next byte alone decides. Most stored history is ASCII, and every width
    // change walks all of it here twice (rewrap, then rebuild).
    // `ascii_fast_path_matches_the_scan` checks every pair against the scan.
    let bytes = text.as_bytes();
    if bytes.get(*byte_idx).is_some_and(|&b| is_printable_ascii(b))
        && bytes
            .get(*byte_idx + 1)
            .is_none_or(|&b| is_printable_ascii(b))
    {
        *byte_idx += 1;
        return GraphemeUnit {
            chars: 1,
            wide: false,
            vs16_widened: false,
            force_narrow: false,
        };
    }
    advance_grapheme_unit_scan(text, byte_idx)
}

/// The char starting at `byte_idx`, a char boundary inside `text`. An ASCII
/// byte is its own char; anything else decodes through `chars()`. The unit
/// walkers (materialize, fill, rewrap) read every stored char through this.
#[inline]
pub(crate) fn char_at(text: &str, byte_idx: usize) -> char {
    match text.as_bytes()[byte_idx] {
        b @ 0..=0x7F => char::from(b),
        _ => text[byte_idx..]
            .chars()
            .next()
            .expect("invariant: byte_idx < text.len()"),
    }
}

/// `char_width(c) == 0`, answered without the table for printable ASCII
/// (always width 1; `ascii_fast_path_matches_the_scan` pins it).
#[inline]
pub(crate) fn is_zero_width(c: char) -> bool {
    !matches!(c, ' '..='~') && aterm_grapheme::char_width(c) == 0
}

/// A printable ASCII byte (0x20..=0x7E): one char of display width 1.
#[inline]
const fn is_printable_ascii(b: u8) -> bool {
    matches!(b, 0x20..=0x7E)
}

/// The general grapheme-unit scan behind [`advance_grapheme_unit_wide`].
fn advance_grapheme_unit_scan(text: &str, byte_idx: &mut usize) -> GraphemeUnit {
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
    // The most recently absorbed scalar, for the flag-pair join below (a second
    // regional indicator folds onto the preceding one). The skin-tone test uses
    // the unit base `c` instead, so an intervening zero-width scalar (VS16,
    // combining) does not defeat the fold.
    let mut prev_c = c;
    // A flag is exactly TWO regional indicators; a third starts a new unit.
    let mut absorbed_ri = false;

    for (offset, next_c) in iter {
        // Bound the unit length. `text` can come from a crafted checkpoint or
        // injected scrollback `Line` (no per-line content cap), so an endless
        // ZWJ/combining chain would otherwise become one huge `complex_char`
        // (`Arc::from(unit_str)`), amplifying memory in materialize, reflow and
        // render. Stop at the ceiling; the rest forms later units (zero-width
        // ones are skipped, overflow past the grid width is dropped).
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
            // A Fitzpatrick modifier folds onto the unit's emoji base, as the
            // live `try_combine_skin_tone_modifier` does (gated on the previous
            // cell being wide and `is_emoji_modifier_base(base)`). The effective
            // width is used, so `☝️🏽` (base + widening VS16 + modifier) folds
            // while text-presentation `☝🏽` stays split.
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
/// Takes `unit_str` as a slice borrowed from the source text;
/// `Arc::from(unit_str)` is used only for complex characters.
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
            // The spacer inherits the lead's rendition (see
            // `wide_continuation_of`). Scroll-off drops spacers and re-derives
            // them here, so a bare role bit would strip e.g. SGR 7 from the
            // glyph's right half in history.
            row.cells[(col + 1) as usize] =
                Cell::with_style(' ', fg, bg, cell_flags.wide_continuation_of());
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
                Cell::with_style(' ', fg, bg, cell_flags.wide_continuation_of());
            col.saturating_add(2)
        } else {
            col
        }
    } else {
        row.cells[col as usize] = Cell::with_style(c, fg, bg, cell_flags);
        col.saturating_add(1)
    }
}

/// Mirror a wide lead's rendition-valued extras onto its continuation spacer.
///
/// The companion to [`CellFlags::wide_continuation_of`](crate::CellFlags::wide_continuation_of),
/// which carries the lead's rendition flags to a re-derived spacer; this
/// carries the values those flags select that do not fit in the flag word:
///
/// - the truecolor fg/bg (a `PackedColor` holds only an overflow sentinel;
///   the 24-bit triple lives in the `CellExtra`);
/// - the SGR 58 underline colour, read by the renderer only when `UNDERLINE`
///   is set (`render_cells.rs`).
///
/// Without it, a scrolled-off wide glyph's right half would underline or
/// (with INVERSE) fill in the wrong colour.
///
/// Derived rather than serialized: a `Line` is per-character, and spacer
/// columns carry no text or `CellAttrs`, so there is no slot for them;
/// deriving also heals history written without it.
///
/// Only fills an empty slot: a span-shaped extra (e.g. a hyperlink span,
/// whose `end_col` is the next non-spacer column) may already cover the
/// spacer, and leaving it keeps the pass idempotent.
///
/// Extended flags (bits 11-13) live in the cell's flag word inside
/// `SPACER_RENDITION_MASK`, so `wide_continuation_of` already carried them.
///
/// O(cols): one flag test per column and at most one map insert per spacer.
fn mirror_wide_spacer_extras(cells: &[crate::Cell], extras: &mut FxHashMap<u16, CellExtra>) {
    if extras.is_empty() {
        return;
    }
    for idx in 1..cells.len() {
        // The same predicate the extractor used to drop this column, so the
        // columns healed are exactly those that lost their extras.
        if !is_spacer(cells, idx) {
            continue;
        }
        let (Ok(lead), Ok(spacer)) = (u16::try_from(idx - 1), u16::try_from(idx)) else {
            break;
        };
        let Some(src) = extras.get(&lead) else {
            continue;
        };
        let fg = src.fg_rgb();
        let bg = src.bg_rgb();
        let underline = src.underline_color_u32();
        if fg.is_none() && bg.is_none() && underline.is_none() {
            continue;
        }
        let dst = extras.entry(spacer).or_default();
        if fg.is_some() && dst.fg_rgb().is_none() {
            dst.set_fg_rgb(fg);
        }
        if bg.is_some() && dst.bg_rgb().is_none() {
            dst.set_bg_rgb(bg);
        }
        if underline.is_some() && dst.underline_color_u32().is_none() {
            dst.set_underline_color_u32(underline);
        }
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
/// Total column-writes are bounded to O(cols): each cell holds at most one
/// hyperlink, so a legitimate (disjoint) line stays under the budget, while a
/// crafted Line with many overlapping full-width spans would otherwise cost
/// O(spans * cols) on the scrollback render path.
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
/// Like [`restore_hyperlinks`]: fills each span's `[start_col, end_col)` with
/// the packed colour via `set_underline_color_u32` (re-deriving the
/// RGB-vs-indexed form so indexed colours resolve against the live palette),
/// with the same O(cols) write budget against overlapping spans.
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
/// Expands each span back to the per-cell [`ImageRef`](crate::ImageRef)s a
/// live image cell has, so a scrolled-back picture takes the same draw path as
/// one on screen. The payload `Arc` is cloned, never the raster, so every
/// restored cell points at the one allocation the renderer's decode cache is
/// keyed on.
fn restore_images(extras: &mut FxHashMap<u16, CellExtra>, line: &Line, cols: u16) {
    if let Some(spans) = line.images() {
        restore_image_spans(extras, spans, cols);
    }
}

/// The body of [`restore_images`], over spans from either source — shared for
/// the same reason as [`restore_hyperlink_spans`].
///
/// Each span's cells get its layer back, stacked in draw order. Bounds total
/// column-writes to O(cols × [`MAX_IMAGE_LAYERS`]) against a stored `Line`
/// carrying more overlapping spans than a cell can stack (the same
/// crafted-input guard as the hyperlink and underline restores).
fn restore_image_spans(extras: &mut FxHashMap<u16, CellExtra>, spans: &[ImageSpan], cols: u16) {
    let mut budget = usize::from(cols) * crate::extra::MAX_IMAGE_LAYERS;
    'restore: for span in spans {
        for icol in span.start_col..span.end_col.min(cols) {
            if budget == 0 {
                break 'restore;
            }
            if let Some(image) = span_layer_at(span, icol) {
                extras.entry(icol).or_default().stack_image(image);
            }
            budget -= 1;
        }
    }
}

/// The image layer `span` paints at column `col`, if it covers it.
pub(super) fn span_layer_at(span: &ImageSpan, col: u16) -> Option<ImageRef> {
    let (cell_row, cell_col) = span.tile_at(col)?;
    Some(ImageRef {
        image: Arc::clone(&span.image),
        cell_row,
        cell_col,
        kitty: span.kitty,
    })
}

#[cfg(test)]
mod ascii_fast_path_tests {
    use super::{GraphemeUnit, advance_grapheme_unit_scan, advance_grapheme_unit_wide};

    fn fields(u: GraphemeUnit) -> (usize, bool, bool, bool) {
        (u.chars, u.wide, u.vs16_widened, u.force_narrow)
    }

    /// The printable-ASCII fast path returns exactly what the general scan
    /// does, for every ASCII first char against every ASCII, a sample of
    /// joining and wide second chars, and the end of the text, each followed
    /// by a tail that would join if the scan reached it.
    #[test]
    fn ascii_fast_path_matches_the_scan() {
        // The width facts both fast paths rest on.
        for c in (0u8..0x80).map(char::from) {
            assert_eq!(
                super::is_zero_width(c),
                aterm_grapheme::char_width(c) == 0,
                "{c:?}"
            );
            if (' '..='~').contains(&c) {
                assert_eq!(aterm_grapheme::char_width(c), 1, "{c:?}");
            }
        }
        let mut seconds: Vec<Option<char>> = (0u8..0x80).map(|b| Some(char::from(b))).collect();
        seconds.extend(
            [
                '\u{0301}',
                '\u{200D}',
                '\u{FE0F}',
                '\u{FE0E}',
                '\u{1F3FD}',
                '\u{1F1FA}',
                '\u{E0001}',
                '\u{65E5}',
                '\u{00E9}',
                '\u{0080}',
            ]
            .map(Some),
        );
        seconds.push(None);
        for first in (0u8..0x80).map(char::from) {
            for &second in &seconds {
                for tail in ["", "\u{0301}", "\u{200D}A", "x"] {
                    for prefix in ["", "q"] {
                        let mut text = String::from(prefix);
                        text.push(first);
                        text.extend(second);
                        text.push_str(tail);
                        let (mut fast_idx, mut scan_idx) = (prefix.len(), prefix.len());
                        let fast = advance_grapheme_unit_wide(&text, &mut fast_idx);
                        let scan = advance_grapheme_unit_scan(&text, &mut scan_idx);
                        assert_eq!(
                            (fields(fast), fast_idx),
                            (fields(scan), scan_idx),
                            "fast path disagrees with the scan on {text:?} at {}",
                            prefix.len()
                        );
                    }
                }
            }
        }
    }
}
