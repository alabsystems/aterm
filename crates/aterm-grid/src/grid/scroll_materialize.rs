// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Full-fidelity scrollback materialization: scrollback [`Line`]s become [`MaterializedRow`]s
//! whose cells and sparse [`CellExtra`]s render as visible-area cells do.

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

/// A scrollback row materialized for rendering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterializedRow {
    /// The cells for this row (one per column).
    pub cells: Vec<super::Cell>,
    /// Sparse extras for the columns that need them.
    extras: FxHashMap<u16, CellExtra>,
    /// Soft-wrap flag from the source [`Line`], as a live `Row` reports it (`row_is_wrapped`).
    wrapped: bool,
    /// The cells reach all but the last column, the shape an early-wrap hole leaves.
    pub(in crate::grid) hole: bool,
}

impl MaterializedRow {
    /// Look up extras for a column (mirrors `CellExtras::get`).
    #[must_use]
    #[inline]
    pub fn get_extra(&self, col: u16) -> Option<&CellExtra> {
        if self.extras.is_empty() {
            return None;
        }
        self.extras.get(&col)
    }

    /// Iterate this row's populated extras as `(col, extra)`, in arbitrary order.
    #[inline]
    pub fn extras_iter(&self) -> impl Iterator<Item = (u16, &CellExtra)> + '_ {
        self.extras.iter().map(|(&col, extra)| (col, extra))
    }

    /// Whether this row is a soft-wrap continuation of the previous line.
    #[must_use]
    #[inline]
    pub(crate) const fn is_wrapped(&self) -> bool {
        self.wrapped
    }

    /// Paint the early-wrap hole as the print path does.
    pub(crate) fn paint_hole(&mut self, glyph: super::Cell, bg_rgb: Option<[u8; 3]>) {
        let Some(col) = self.cells.len().checked_sub(1).filter(|_| self.hole) else {
            return;
        };
        let blank = super::Cell::bce_blank(glyph.colors());
        self.cells[col] = blank;
        if blank.colors().bg_is_rgb()
            && let Some(rgb) = bg_rgb
        {
            self.extras
                .entry(u16::try_from(col).unwrap_or(u16::MAX))
                .or_default()
                .set_bg_rgb(Some(rgb));
        }
    }

    /// How many columns the row's text spans.
    #[must_use]
    #[inline]
    pub(crate) fn written_len(&self) -> u16 {
        let len = self.len();
        if self.hole {
            len.min(u16::try_from(self.cells.len().saturating_sub(1)).unwrap_or(u16::MAX))
        } else {
            len
        }
    }

    /// Whether this row has no occupied columns.
    #[cfg(test)]
    #[must_use]
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Effective row length (last occupied column + 1), matching `Row::len`.
    #[must_use]
    #[inline]
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

/// Whether `filled` columns of a `cols`-wide row leave the last cell as an early-wrap hole.
#[inline]
pub(crate) const fn leaves_wrap_hole(filled: u16, cols: u16) -> bool {
    (filled as u32) + 1 == (cols as u32)
}

/// Materialize a scrollback [`Line`] into a [`MaterializedRow`], restoring hyperlinks.
#[must_use]
pub fn materialize_from_line(line: &Line, cols: u16) -> MaterializedRow {
    let mut row = MaterializedRow {
        cells: vec![super::Cell::default(); cols as usize],
        extras: FxHashMap::default(),
        wrapped: line.is_wrapped(),
        hole: false,
    };

    let Some(text) = line.as_str() else {
        return row;
    };

    let mut walk = StoredCells::new(line, text, cols);
    for cell in walk.by_ref() {
        let flags = CellFlags::from_bits(cell.attrs.flags);
        let is_complex = cell.chars > 1 || cell.base as u32 > super::Cell::MAX_DIRECT_CODEPOINT;
        let fg = PackedColor(cell.attrs.fg);
        let bg = PackedColor(cell.attrs.bg);
        place_cell(
            &mut row, cell.col, cols, cell.base, cell.text, fg, bg, flags, cell.wide, is_complex,
        );
        store_rgb_extras(&mut row.extras, cell.col, cols, &cell.attrs);
    }
    let filled = walk.col;
    row.hole = leaves_wrap_hole(filled, cols);

    restore_hyperlinks(&mut row.extras, line, filled);

    restore_underline_colors(&mut row.extras, line, filled);

    restore_images(&mut row.extras, line, cols);

    mirror_wide_spacer_extras(&row.cells, &mut row.extras);

    row
}

/// The cells a stored line's text is drawn in on a row `cols` wide.
pub fn stored_line_cells<'a>(
    line: &'a Line,
    text: &'a str,
    cols: u16,
) -> impl Iterator<Item = (std::ops::Range<usize>, u16, u16, u16)> + 'a {
    let mut walk = StoredCells::new(line, text, cols);
    walk.ascii_runs = !line.attrs().is_some_and(|attrs| {
        attrs
            .runs()
            .iter()
            .any(|run| CellFlags::from_bits(run.value.flags).contains(CellFlags::WIDE))
    });
    std::iter::from_fn(move || {
        if let Some((byte, col, count)) = walk.ascii_run() {
            return Some((byte..byte.saturating_add(1), col, 1, count));
        }
        walk.next().map(|cell| {
            let end = cell.byte.saturating_add(cell.text.len());
            (cell.byte..end, cell.col, if cell.wide { 2 } else { 1 }, 1)
        })
    })
}

/// The walk of a stored line's text into grapheme units, one per cell.
pub(in crate::grid) struct StoredUnits<'a> {
    text: &'a str,
    attrs: aterm_scrollback::AttrRunCursor<'a>,
    openings: CellOpenings<'a>,
    byte_idx: usize,
    char_idx: usize,
}

/// One unit of a stored line (see [`StoredUnits`]).
pub(in crate::grid) struct StoredUnit<'a> {
    /// The unit's text.
    pub(in crate::grid) text: &'a str,
    /// Where `text` starts in the line.
    pub(in crate::grid) byte: usize,
    /// The unit's first character, the cell's base.
    pub(in crate::grid) base: char,
    /// The unit as scanned: its characters and effective width.
    pub(in crate::grid) unit: GraphemeUnit,
    /// The unit's stored attributes, [`CellAttrs::OPENS_CELL`] cleared.
    pub(in crate::grid) attrs: CellAttrs,
}

impl<'a> StoredUnits<'a> {
    /// Walk `text`, `line`'s text as the caller read it.
    pub(in crate::grid) fn new(line: &'a Line, text: &'a str) -> Self {
        Self {
            text,
            attrs: line.attr_cursor(),
            openings: CellOpenings::new(line),
            byte_idx: 0,
            char_idx: 0,
        }
    }
}

impl<'a> Iterator for StoredUnits<'a> {
    type Item = StoredUnit<'a>;

    fn next(&mut self) -> Option<StoredUnit<'a>> {
        while self.byte_idx < self.text.len() {
            let c = char_at(self.text, self.byte_idx);
            if is_zero_width(c) {
                self.byte_idx += c.len_utf8();
                self.char_idx += 1;
                continue;
            }
            let byte = self.byte_idx;
            let char_start = self.char_idx;
            let limit = self.openings.limit(char_start);
            let unit = advance_grapheme_unit_within(self.text, &mut self.byte_idx, limit);
            self.char_idx += unit.chars;
            let attrs = self.attrs.attr_at(char_start).with_opens_cell(false);
            return Some(StoredUnit {
                text: &self.text[byte..self.byte_idx],
                byte,
                base: c,
                unit,
                attrs,
            });
        }
        None
    }
}

/// Where a stored line's [`CellAttrs::OPENS_CELL`] marks cut its walk, read
/// off the attribute runs for non-decreasing unit starts in O(runs) in all.
struct CellOpenings<'a> {
    runs: &'a [aterm_rle::Run<CellAttrs>],
    /// The run the next search resumes at, and the character it starts on.
    i: usize,
    run_start: usize,
    /// The first marked character at or after the last search's start (`usize::MAX` for none).
    next: usize,
}

impl<'a> CellOpenings<'a> {
    fn new(line: &'a Line) -> Self {
        Self {
            runs: line.attrs().map_or(&[], aterm_rle::Rle::runs),
            i: 0,
            run_start: 0,
            next: 0,
        }
    }

    /// How many characters the unit starting at character `start` may take:
    /// up to the next character after it that opens a cell.
    fn limit(&mut self, start: usize) -> usize {
        let from = start.saturating_add(1);
        if self.next < from {
            self.next = usize::MAX;
            while let Some(run) = self.runs.get(self.i) {
                let end = self.run_start.saturating_add(run.length as usize);
                if end > from && run.value.opens_cell() {
                    self.next = self.run_start.max(from);
                    break;
                }
                self.run_start = end;
                self.i += 1;
            }
        }
        self.next - start
    }
}

/// One cell of a stored line (see [`StoredCells`]).
pub(in crate::grid) struct StoredCell<'a> {
    /// The cell's text: its grapheme unit.
    pub(in crate::grid) text: &'a str,
    /// Where `text` starts in the line.
    pub(in crate::grid) byte: usize,
    /// The unit's first character, the cell's base.
    pub(in crate::grid) base: char,
    /// Characters in the unit.
    pub(in crate::grid) chars: usize,
    /// The unit's stored attributes.
    pub(in crate::grid) attrs: CellAttrs,
    /// The cell's first column.
    pub(in crate::grid) col: u16,
    /// Drawn two columns wide.
    pub(in crate::grid) wide: bool,
}

/// The layout walk over a stored line's text.
pub(in crate::grid) struct StoredCells<'a> {
    units: StoredUnits<'a>,
    /// The column after the last cell laid out.
    pub(in crate::grid) col: u16,
    cols: u16,
    /// Whether [`Self::ascii_run`] may step over a run of printable ASCII.
    ascii_runs: bool,
}

impl<'a> StoredCells<'a> {
    pub(in crate::grid) fn new(line: &'a Line, text: &'a str, cols: u16) -> Self {
        Self {
            units: StoredUnits::new(line, text),
            col: 0,
            cols,
            ascii_runs: false,
        }
    }

    /// Step over the printable ASCII cells at the walk's position at once,
    /// returning the first one's byte, its column and the count.
    fn ascii_run(&mut self) -> Option<(usize, u16, u16)> {
        if !self.ascii_runs {
            return None;
        }
        let walk = &mut self.units;
        let rest = walk.text.as_bytes().get(walk.byte_idx..)?;
        let span = rest.iter().take_while(|&&b| is_printable_ascii(b)).count();
        let units = if span == rest.len() {
            span
        } else {
            span.saturating_sub(1)
        };
        let room = self.cols.saturating_sub(self.col);
        let count = u16::try_from(units).unwrap_or(u16::MAX).min(room);
        if count == 0 {
            return None;
        }
        let (byte, col) = (walk.byte_idx, self.col);
        walk.byte_idx = byte.saturating_add(usize::from(count));
        walk.char_idx = walk.char_idx.saturating_add(usize::from(count));
        self.col = col.saturating_add(count);
        Some((byte, col, count))
    }
}

impl<'a> Iterator for StoredCells<'a> {
    type Item = StoredCell<'a>;

    fn next(&mut self) -> Option<StoredCell<'a>> {
        if self.col >= self.cols {
            return None;
        }
        let StoredUnit {
            text,
            byte,
            base,
            unit,
            attrs,
        } = self.units.next()?;
        let col = self.col;
        let last = col.saturating_add(1) >= self.cols;
        let wide = stored_unit_is_wide(unit, attrs) && !(unit.vs16_widened && last);
        if wide && last {
            self.cols = col;
            return None;
        }
        self.col = col.saturating_add(if wide { 2 } else { 1 });
        Some(StoredCell {
            text,
            byte,
            base,
            chars: unit.chars,
            attrs,
            col,
            wide,
        })
    }
}

/// Materialize a ring-tier history row straight from its stored `Row` and `ScrolledRowExtras`.
#[must_use]
pub(in crate::grid) fn materialize_from_row_extras(
    row: &Row,
    extras: Option<&ScrolledRowExtras>,
    cols: u16,
) -> Option<MaterializedRow> {
    use crate::Cell;

    let no_extras = ScrolledRowExtras::default();
    let extras = extras.unwrap_or(&no_extras);
    let mut walk = RingCells::new(row, extras, cols)?;

    let mut out = MaterializedRow {
        cells: vec![Cell::default(); cols as usize],
        extras: FxHashMap::default(),
        wrapped: row.is_wrapped(),
        hole: false,
    };
    if row.is_empty() {
        restore_image_spans(&mut out.extras, &extras.images, cols);
        #[cfg(any(test, debug_assertions))]
        super::count_ring_fast_materialize();
        return Some(out);
    }

    while let Some(cell) = walk.next_cell().ok()? {
        let end_col = place_cell(
            &mut out,
            cell.col,
            cols,
            cell.base,
            cell.text,
            PackedColor(cell.fg_raw),
            PackedColor(cell.bg_raw),
            CellFlags::from_bits(cell.attrs.flags),
            cell.wide,
            cell.complex,
        );
        debug_assert_eq!(end_col, cell.end_col, "the walk places as place_cell does");
        store_rgb_extras(&mut out.extras, cell.col, cols, &cell.attrs);
    }

    let col = walk.col;
    out.hole = leaves_wrap_hole(col, cols);
    restore_hyperlink_spans(&mut out.extras, &extras.hyperlinks, col);
    if !extras.underline_colors.is_empty() {
        restore_underline_color_spans(
            &mut out.extras,
            &coalesce_underline_spans(&extras.underline_colors),
            col,
        );
    }
    restore_image_spans(&mut out.extras, &extras.images, cols);
    mirror_wide_spacer_extras(&out.cells, &mut out.extras);

    #[cfg(any(test, debug_assertions))]
    super::count_ring_fast_materialize();
    Some(out)
}

/// One cell of a ring-tier row as the ring fast path draws it (see
/// [`RingCells`]).
struct RingCell<'w> {
    /// The cell's first column.
    col: u16,
    /// The column after it.
    end_col: u16,
    /// The unit's first character, the cell's base.
    base: char,
    /// The cell's text, exactly as the `Line` serializer writes the cell.
    text: &'w str,
    /// The cell's foreground, as the serializer resolves it.
    fg_raw: u32,
    /// The cell's background, as the serializer resolves it.
    bg_raw: u32,
    /// The cell's attributes, as the serializer stores them.
    attrs: CellAttrs,
    /// Drawn two columns wide.
    wide: bool,
    /// Held as a cluster string rather than inline.
    complex: bool,
}

/// The ring fast path cannot prove it draws the row as the round trip does.
struct Declined;

/// The walk over a ring-tier row's cells that the ring fast path draws: each
/// non-spacer cell as one grapheme unit at its own column, as wide as the
/// round trip would place it, or [`Declined`] where the round trip draws the
/// row otherwise.
struct RingCells<'a> {
    /// The row's cells up to its length.
    cells: &'a [crate::Cell],
    extras: &'a ScrolledRowExtras,
    cursors: RowToLineCursorState,
    /// The next source cell.
    physical: usize,
    /// The column the next unit is placed at.
    col: u16,
    cols: u16,
    /// A plain cell's one scalar, encoded.
    char_buf: [u8; 4],
    /// The text of a cell that needs a cluster string.
    unit_buf: String,
}

impl<'a> RingCells<'a> {
    /// The walk over `row`, or `None` for a row wider than `cols`: the round
    /// trip clips it, which this path does not model (rare: both come from one
    /// grid).
    fn new(row: &'a Row, extras: &'a ScrolledRowExtras, cols: u16) -> Option<Self> {
        let len = usize::from(row.written_len());
        if len > usize::from(cols) {
            return None;
        }
        Some(Self {
            cells: &row.as_slice()[..len],
            extras,
            cursors: RowToLineCursorState::default(),
            physical: 0,
            col: 0,
            cols,
            char_buf: [0; 4],
            unit_buf: String::new(),
        })
    }

    /// The next cell, `Ok(None)` after the last, or [`Declined`].
    #[inline]
    #[allow(
        clippy::too_many_lines,
        reason = "one linear transcription of the Line round trip's per-cell derivation; \
                  splitting it would put the two halves of a parity argument in two places"
    )]
    fn next_cell(&mut self) -> Result<Option<RingCell<'_>>, Declined> {
        use crate::Cell;

        while self.physical < self.cells.len() && is_spacer(self.cells, self.physical) {
            self.physical += 1;
        }
        let Some(&cell) = self.cells.get(self.physical) else {
            return if usize::from(self.col) == self.cells.len() {
                Ok(None)
            } else {
                Err(Declined)
            };
        };
        let col_u16 = u16::try_from(self.physical).map_err(|_| Declined)?;
        self.physical += 1;
        if self.col != col_u16 {
            return Err(Declined);
        }

        let plain = if cell.is_complex() {
            None
        } else {
            let ch = cell.char();
            Some(if ch == '\0' { ' ' } else { ch })
        };
        let stored = if cell.is_complex() {
            next_complex_char(self.extras, &mut self.cursors, col_u16)
        } else {
            None
        };
        let marks = next_combining(self.extras, &mut self.cursors, col_u16);

        let base: char;
        let unit: GraphemeUnit;
        let one_scalar = matches!((plain, marks), (Some(_), None));
        if let (Some(ch), None) = (plain, marks) {
            base = ch;
            unit = GraphemeUnit {
                chars: 1,
                wide: aterm_grapheme::char_width(ch) >= 2,
                vs16_widened: false,
                force_narrow: false,
            };
        } else {
            self.unit_buf.clear();
            match (plain, stored) {
                (Some(ch), _) => self.unit_buf.push(ch),
                (None, Some(value)) => self.unit_buf.push_str(value),
                (None, None) => self.unit_buf.push('\u{FFFD}'),
            }
            if let Some(marks) = marks {
                self.unit_buf.extend(marks.iter().copied());
            }
            base = self.unit_buf.chars().next().ok_or(Declined)?;
            let mut scanned = 0usize;
            unit = advance_grapheme_unit_wide(&self.unit_buf, &mut scanned);
            if scanned != self.unit_buf.len() {
                // The scan stopped early: the round trip would emit a second
                // unit for the tail, or clip the cluster at
                // MAX_GRAPHEME_UNIT_BYTES. Give up on the row.
                return Err(Declined);
            }
        }
        if aterm_grapheme::char_width(base) == 0 {
            // The text walk skips a zero-width base and shifts later columns
            // left; not modelled here.
            return Err(Declined);
        }

        let fg_raw = resolve_cell_color(
            cell.fg_needs_overflow(),
            cell.fg_color().map_or(PackedColor::DEFAULT_FG.0, |c| c.0),
            &self.extras.rgb_fg,
            &mut self.cursors.rgb_fg_idx,
            col_u16,
            PackedColor::DEFAULT_FG.0,
        );
        let bg_raw = resolve_cell_color(
            cell.bg_needs_overflow(),
            cell.bg_color().map_or(PackedColor::DEFAULT_BG.0, |c| c.0),
            &self.extras.rgb_bg,
            &mut self.cursors.rgb_bg_idx,
            col_u16,
            PackedColor::DEFAULT_BG.0,
        );
        let attrs = CellAttrs::from_raw(fg_raw, bg_raw, cell.flags().bits());
        let wide =
            stored_unit_is_wide(unit, attrs) && !(unit.vs16_widened && col_u16 + 1 >= self.cols);
        let complex = unit.chars > 1 || base as u32 > Cell::MAX_DIRECT_CODEPOINT;

        let end_col = if !wide {
            col_u16.saturating_add(1)
        } else if col_u16 + 1 < self.cols {
            col_u16.saturating_add(2)
        } else {
            // No progress: a wide unit cannot fit at the last column, and the
            // text walk stops there, dropping the rest of the row.
            return Err(Declined);
        };
        self.col = end_col;
        let text: &str = if one_scalar {
            base.encode_utf8(&mut self.char_buf)
        } else {
            &self.unit_buf
        };
        Ok(Some(RingCell {
            col: col_u16,
            end_col,
            base,
            text,
            fg_raw,
            bg_raw,
            attrs,
            wide,
            complex,
        }))
    }
}

/// Byte ceiling for one grapheme unit read from a stored `Line`
/// ([`advance_grapheme_unit_wide`]).
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

/// Advance `byte_idx` past the current grapheme unit and return the characters consumed.
#[cfg(test)]
pub(crate) fn advance_grapheme_unit(text: &str, byte_idx: &mut usize) -> usize {
    advance_grapheme_unit_wide(text, byte_idx).chars
}

/// One scanned grapheme unit.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GraphemeUnit {
    /// Characters consumed (callers advance their parallel char index by this).
    pub(crate) chars: usize,
    /// The unit's effective width is 2 cells (after VS16/VS15 replay).
    pub(crate) wide: bool,
    /// `wide` is true only because VS16 widened a narrow base.
    pub(crate) vs16_widened: bool,
    /// The final width was narrowed by VS15, overriding a stale or authored `WIDE` attribute.
    force_narrow: bool,
}

/// Resolve a Line-backed unit's write-time cell geometry.
#[inline]
pub(crate) const fn stored_unit_is_wide(unit: GraphemeUnit, attrs: CellAttrs) -> bool {
    let stored_wide = CellFlags::from_bits(attrs.flags).contains(CellFlags::WIDE);
    !unit.force_narrow && (stored_wide || (unit.wide && !unit.vs16_widened))
}

/// Advance `byte_idx` past the current grapheme unit.
pub(crate) fn advance_grapheme_unit_wide(text: &str, byte_idx: &mut usize) -> GraphemeUnit {
    advance_grapheme_unit_within(text, byte_idx, usize::MAX)
}

/// [`advance_grapheme_unit_wide`] taking at most `max_chars` (at least 1)
/// characters: a stored line's walk ends a unit before a character that opens
/// a cell ([`StoredUnits`]).
fn advance_grapheme_unit_within(
    text: &str,
    byte_idx: &mut usize,
    max_chars: usize,
) -> GraphemeUnit {
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
    advance_grapheme_unit_scan(text, byte_idx, max_chars)
}

/// Whether a walk of `text` from `prev` attaches a variation selector.
pub(crate) fn walk_folds_across(text: &str, prev: usize, at: usize) -> bool {
    let Some(&lead) = text.as_bytes().get(at) else {
        return false;
    };
    if lead.is_ascii() {
        return false;
    }
    let c = char_at(text, at);
    let may_fold =
        is_regional_indicator(c) || is_skin_tone_modifier(c) || text[..at].ends_with('\u{200D}');
    if !may_fold || is_zero_width(c) {
        return false;
    }
    let mut idx = prev;
    while idx < at {
        let before = idx;
        advance_grapheme_unit_wide(text, &mut idx);
        if idx == before {
            return false;
        }
    }
    idx > at
}

/// The char starting at `byte_idx`, a char boundary inside `text`.
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

/// `char_width(c) == 0`, answered without the table for printable ASCII.
#[inline]
pub(crate) const fn is_zero_width(c: char) -> bool {
    !matches!(c, ' '..='~') && aterm_grapheme::char_width(c) == 0
}

/// A printable ASCII byte (0x20..=0x7E): one char of display width 1.
#[inline]
const fn is_printable_ascii(b: u8) -> bool {
    matches!(b, 0x20..=0x7E)
}

/// The general grapheme-unit scan behind [`advance_grapheme_unit_wide`].
fn advance_grapheme_unit_scan(text: &str, byte_idx: &mut usize, max_chars: usize) -> GraphemeUnit {
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
    let mut wide = aterm_grapheme::char_width(c) >= 2;
    let mut vs16_widened = false;
    let mut force_narrow = false;
    let mut ris = u8::from(is_regional_indicator(c));

    for (offset, next_c) in iter {
        // Bound the unit: `text` can come from a crafted checkpoint or injected
        // `Line`, and an endless ZWJ/combining chain would otherwise become one
        // huge `complex_char`. The rest forms later units.
        if end >= MAX_GRAPHEME_UNIT_BYTES || char_count >= max_chars {
            break;
        }
        let next_width = aterm_grapheme::char_width(next_c);

        if next_c == '\u{200D}' {
            last_was_zwj = true;
        } else if next_c == '\u{FE0F}' {
            // VS16 joins the unit (zero-width) and widens a narrow emoji-capable
            // base, as the live `widen_previous_cell_for_vs16` does.
            if !wide && aterm_grapheme::is_vs16_emoji_capable(c) {
                wide = true;
                vs16_widened = true;
                force_narrow = false;
            }
            last_was_zwj = false;
        } else if next_c == '\u{FE0E}' {
            wide = false;
            vs16_widened = false;
            force_narrow = true;
            last_was_zwj = false;
        } else if next_width == 0 || (last_was_zwj && !next_c.is_ascii()) {
            // Zero-width chars (combining marks, other selectors) join the
            // unit, and so does a visible non-ASCII char after a ZWJ. Printable
            // ASCII after a ZWJ opens its own cell, as the live grid draws it
            // (`print_ascii_bulk` never folds) and `aterm_grapheme::GridColumns`
            // lays it out. Known gap: under insert mode, no autowrap or a
            // translating charset the live writer takes ASCII through
            // `write_char`, which does fold it after a ZWJ, and a `Line` does
            // not record the mode.
            last_was_zwj = false;
        } else if is_skin_tone_modifier(next_c) && aterm_grapheme::is_emoji_modifier_base(c) && wide
        {
            // A Fitzpatrick modifier folds onto the unit's emoji base, as the
            // live `try_combine_skin_tone_modifier` does (gated on the previous
            // cell being wide and `is_emoji_modifier_base(base)`). The effective
            // width is used, so `☝️🏽` (base + widening VS16 + modifier) folds
            // while text-presentation `☝🏽` stays split.
            last_was_zwj = false;
        } else if is_regional_indicator(next_c) && is_regional_indicator(c) && ris < 2 {
            // A regional indicator folds onto a unit whose base is a lone one,
            // as `try_combine_regional_indicator` reads the cell's base and
            // counts the indicators it holds: a mark between (`🇺́🇸`) does not
            // stop the fold, as `aterm_grapheme::GridColumns` lays it out too.
            last_was_zwj = false;
        } else {
            break;
        }

        end = offset + next_c.len_utf8();
        char_count += 1;
        ris = ris.saturating_add(u8::from(is_regional_indicator(next_c)));
    }

    *byte_idx += end;
    GraphemeUnit {
        chars: char_count,
        wide,
        vs16_widened,
        force_narrow,
    }
}

/// Place a cell (complex, wide, or normal) into the materialized row and return
/// the column after it.
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
        let Some(values) = extras.get(&lead).and_then(SpacerValues::of) else {
            continue;
        };
        values.fill(extras.entry(spacer).or_default());
    }
}

/// The values a wide lead's continuation spacer inherits from it.
#[derive(Clone, Copy)]
pub(super) struct SpacerValues {
    fg: Option<[u8; 3]>,
    bg: Option<[u8; 3]>,
    underline: Option<u32>,
}

impl SpacerValues {
    /// What `lead` hands its spacer, or `None` when it carries none of them.
    pub(super) fn of(lead: &CellExtra) -> Option<Self> {
        let values = Self {
            fg: lead.fg_rgb(),
            bg: lead.bg_rgb(),
            underline: lead.underline_color_u32(),
        };
        (values.fg.is_some() || values.bg.is_some() || values.underline.is_some()).then_some(values)
    }

    /// Fill the spacer's empty slots.
    pub(super) const fn fill(self, spacer: &mut CellExtra) {
        if self.fg.is_some() && spacer.fg_rgb().is_none() {
            spacer.set_fg_rgb(self.fg);
        }
        if self.bg.is_some() && spacer.bg_rgb().is_none() {
            spacer.set_bg_rgb(self.bg);
        }
        if self.underline.is_some() && spacer.underline_color_u32().is_none() {
            spacer.set_underline_color_u32(self.underline);
        }
    }
}

/// Store a placed cell's truecolor fg/bg from its `CellAttrs` in the extras.
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

/// Restore a line's hyperlinks onto the cells of its first `filled` columns
/// ([`filled_span_cols`]), with column writes bounded to O(cols): a legitimate
/// line's spans are disjoint, and a crafted one with many overlapping
/// full-width spans would otherwise cost O(spans × cols) on the render path.
fn restore_hyperlinks(extras: &mut FxHashMap<u16, CellExtra>, line: &Line, filled: u16) {
    if let Some(spans) = line.hyperlinks() {
        restore_hyperlink_spans(extras, spans, filled);
    }
}

/// The body of [`restore_hyperlinks`] over spans from either source (a `Line`'s
/// copy or a ring row's stored vector), shared so the two materialization paths
/// cannot drift on the overlap budget or the end-column bound.
fn restore_hyperlink_spans(
    extras: &mut FxHashMap<u16, CellExtra>,
    spans: &[HyperlinkSpan],
    filled: u16,
) {
    let mut budget = filled;
    'restore: for span in spans {
        for hcol in filled_span_cols(span.start_col, span.end_col, filled) {
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

/// The columns of a stored span (a hyperlink or SGR 58 underline colour, `start_col..end_col`).
pub(super) fn filled_span_cols(start_col: u16, end_col: u16, filled: u16) -> core::ops::Range<u16> {
    start_col..end_col.min(filled)
}

/// Restore a line's SGR 58 underline colours on its first `filled` columns
/// under [`restore_hyperlinks`]' budget; `set_underline_color_u32` re-derives
/// the RGB-or-indexed form so an indexed colour resolves against the live
/// palette.
fn restore_underline_colors(extras: &mut FxHashMap<u16, CellExtra>, line: &Line, filled: u16) {
    if let Some(spans) = line.underline_colors() {
        restore_underline_color_spans(extras, spans, filled);
    }
}

/// The body of [`restore_underline_colors`] over spans from either source,
/// shared as [`restore_hyperlink_spans`] is.
fn restore_underline_color_spans(
    extras: &mut FxHashMap<u16, CellExtra>,
    spans: &[UnderlineColorSpan],
    filled: u16,
) {
    let mut budget = filled;
    'restore: for span in spans {
        for ucol in filled_span_cols(span.start_col, span.end_col, filled) {
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

/// Restore a line's inline images as the per-cell [`ImageRef`](crate::ImageRef)s
/// a live image cell has, so a scrolled-back picture takes the on-screen draw
/// path; every restored cell shares the one payload `Arc` the renderer's decode
/// cache keys on.
fn restore_images(extras: &mut FxHashMap<u16, CellExtra>, line: &Line, cols: u16) {
    if let Some(spans) = line.images() {
        restore_image_spans(extras, spans, cols);
    }
}

/// The body of [`restore_images`] over spans from either source, shared as
/// [`restore_hyperlink_spans`] is.
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

    /// The search walk's ASCII runs are exactly the cells the per-cell walk
    /// lays the line out in, on lines mixing printable ASCII with joining
    /// marks, controls, wide, folding and ZWJ-joined characters, at every row
    /// width, with and without a stored wide bit.
    #[test]
    fn the_search_walk_lays_a_line_out_as_materialize_does() {
        use aterm_rle::Rle;

        use super::{CellAttrs, CellFlags, Line, StoredCells, stored_line_cells};

        let pieces = [
            "ab",
            "a\u{301}",
            "\u{301}",
            "\t",
            "\u{65e5}",
            "\u{2714}\u{fe0f}",
            "\u{231a}\u{fe0e}",
            "\u{1f44d}\u{200d}a",
            "\u{1f1fa}\u{1f1f8}",
            "\u{23bf}",
            " ",
        ];
        let mut texts = Vec::new();
        for a in pieces {
            for b in pieces {
                for c in pieces {
                    texts.push(format!("{a}x{b}yz{c}"));
                }
            }
        }
        for text in &texts {
            let plain = Line::from_bytes(text.as_bytes());
            let mut wide_attrs = Rle::new();
            let wide = CellAttrs {
                flags: CellFlags::WIDE.bits(),
                ..CellAttrs::DEFAULT
            };
            wide_attrs.push(wide);
            wide_attrs.extend_with(
                CellAttrs::DEFAULT,
                u32::try_from(text.chars().count() - 1).unwrap(),
            );
            let styled = Line::with_attrs(text, wide_attrs);
            for line in [&plain, &styled] {
                for cols in 1..=u16::try_from(text.len() + 2).unwrap() {
                    let per_cell: Vec<_> = StoredCells::new(line, text, cols)
                        .map(|cell| (cell.byte, cell.text.len(), cell.col, cell.wide))
                        .collect();
                    let mut runs = Vec::new();
                    for (bytes, col, width, count) in stored_line_cells(line, text, cols) {
                        for k in 0..count {
                            let len = bytes.len();
                            runs.push((
                                bytes.start + usize::from(k) * len,
                                len,
                                col + k * width,
                                width == 2,
                            ));
                        }
                    }
                    assert_eq!(runs, per_cell, "{text:?} at {cols} columns");
                }
            }
        }
    }

    /// The printable-ASCII fast path returns exactly what the general scan
    /// does, for every ASCII first char against every ASCII, a sample of
    /// joining and wide second chars, and the end of the text, each followed by
    /// a tail that would join if the scan reached it.
    #[test]
    fn ascii_fast_path_matches_the_scan() {
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
                        let scan = advance_grapheme_unit_scan(&text, &mut scan_idx, usize::MAX);
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
