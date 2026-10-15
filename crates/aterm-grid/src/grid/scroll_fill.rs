// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Fill grid rows from scrollback lines.
//!
//! Converts scrollback [`Line`]s back to grid cells (Kitty CSI +T unscroll,
//! checkpoint restore, reflow). Uses grapheme-unit iteration (via
//! [`advance_grapheme_unit`](super::scroll_materialize::advance_grapheme_unit))
//! to handle combining marks, ZWJ emoji sequences and other multi-codepoint
//! clusters.

use std::sync::Arc;

use aterm_scrollback::Line;

use super::Grid;
use super::ScrolledRowExtras;
use super::scroll_materialize::filled_span_cols;
use crate::Row;
use crate::StyleTable;
use crate::{CellCoord, CellExtras};

/// Fill a standalone [`Row`] from a scrollback [`Line`] and return the row's
/// preserved extras (complex chars, combining marks, RGB colors, hyperlinks).
///
/// Unlike [`Grid::fill_row_from_line`], which writes the grid's `CellExtras`
/// keyed by visible row, this targets a detached row and collects its extras
/// into a [`ScrolledRowExtras`] (the form ring scrollback rows store). Used by
/// scrollback reflow to rebuild ring scrollback rows at a new width.
///
/// `row` must be fresh from [`Row::new`] (every cell blank, `len` 0, flags
/// `DIRTY`: the state [`Row::clear`] leaves), as its one caller,
/// `Grid::build_scrollback_row`, allocates it: a second clear of every cell
/// per rebuilt history row would be pure overhead on each width change.
///
/// Links and underline colours come back only on the cells the fill put a
/// glyph back in ([`filled_span_cols`]): one column cannot hold a wide glyph,
/// so its rewrapped line rebuilds as an empty row, and the glyph's link must
/// not outlive it on the empty cell.
pub(crate) fn fill_row_into(
    row: &mut Row,
    line: &Line,
    cols: u16,
    _styles: &StyleTable,
) -> ScrolledRowExtras {
    debug_assert_eq!(row.len(), 0, "fill_row_into takes a fresh row");
    let mut extras = ScrolledRowExtras::default();
    let Some(text) = line.as_str() else {
        row.set_wrapped(line.is_wrapped());
        return extras;
    };
    let (deferred, filled) = fill_row_cells(row, text, line, cols);
    row.set_wrapped(line.is_wrapped());
    for d in deferred {
        match d {
            DeferredExtra::CombiningMarks(col, marks) => {
                extras
                    .combining
                    .push((col, aterm_alloc::SmallVec::from_slice(&marks)));
            }
            DeferredExtra::ComplexChar(col, s) => extras.complex_chars.push((col, s)),
            DeferredExtra::RgbFg(col, rgb) => extras.rgb_fg.push((col, rgb)),
            DeferredExtra::RgbBg(col, rgb) => extras.rgb_bg.push((col, rgb)),
        }
    }
    if let Some(spans) = line.hyperlinks() {
        for span in spans {
            let held = filled_span_cols(span.start_col, span.end_col, filled);
            if held.is_empty() {
                continue;
            }
            extras
                .hyperlinks
                .push(aterm_scrollback::HyperlinkSpan::with_id(
                    held.start,
                    held.end,
                    span.url.clone(),
                    span.id.clone(),
                ));
        }
    }
    // Inline images stay in span form (`ScrolledRowExtras` keeps them
    // coalesced): clipping the end column to the new width is the whole
    // conversion, matching a live footprint at a narrower window, whose tail
    // tiles clip at the right margin. An image covers blank cells by design,
    // so the glyph rule of `filled_span_cols` is not its.
    if let Some(spans) = line.images() {
        for span in spans {
            if span.start_col >= cols {
                continue;
            }
            extras.images.push(aterm_scrollback::ImageSpan {
                end_col: span.end_col.min(cols),
                ..span.clone()
            });
        }
    }
    // Expand SGR 58 underline-colour spans back to the per-column form a
    // `ScrolledRowExtras` carries (row_to_line re-coalesces on the next scroll).
    // O(cols) budget guards against crafted overlapping spans, as elsewhere.
    if let Some(spans) = line.underline_colors() {
        let mut budget = cols;
        'ul: for span in spans {
            for ucol in filled_span_cols(span.start_col, span.end_col, filled) {
                if budget == 0 {
                    break 'ul;
                }
                extras.underline_colors.push((ucol, span.color));
                budget -= 1;
            }
        }
    }
    extras
}

impl Grid {
    /// Fill a row with content from a scrollback Line.
    ///
    /// Uses grapheme-unit-aware iteration to handle combining marks, ZWJ
    /// emoji sequences and other multi-codepoint clusters.
    ///
    /// Two phases for the borrow checker: phase 1 fills the row cells (via
    /// `row_mut`), phase 2 fills CellExtras for complex characters after the
    /// row borrow ends.
    pub(crate) fn fill_row_from_line(&mut self, row_idx: u16, line: &Line, cols: u16) {
        let Some(text) = line.as_str() else {
            if let Some(row) = self.row_mut(row_idx) {
                row.clear();
                row.set_wrapped(line.is_wrapped());
            }
            return;
        };

        // Phase 1: populate row cells, collect deferred extras and how far
        // the line's cells reach (the bound on every span restored below).
        let (deferred, filled) = {
            let Some(row) = self.row_mut(row_idx) else {
                return;
            };
            row.clear();
            let result = fill_row_cells(row, text, line, cols);
            row.set_wrapped(line.is_wrapped());
            result
        };

        // Phase 2: populate CellExtras (row borrow released).
        // Collect column indices before consuming `deferred` so we can set HAS_EXTRAS.
        let deferred_cols: aterm_alloc::SmallVec<u16, 4> = deferred
            .iter()
            .map(|d| match d {
                DeferredExtra::CombiningMarks(col, _)
                | DeferredExtra::ComplexChar(col, _)
                | DeferredExtra::RgbFg(col, _)
                | DeferredExtra::RgbBg(col, _) => *col,
            })
            .collect();
        apply_deferred_extras(&mut self.storage.extras, row_idx, deferred);
        // Set HAS_EXTRAS flags on cells that got deferred extras.
        if !deferred_cols.is_empty()
            && let Some(idx) = self.storage.row_index(row_idx)
            && let Some(r) = self.storage.rows.get_mut(idx)
        {
            for col in deferred_cols {
                if let Some(cell) = r.get_mut(col) {
                    cell.set_has_extras(true);
                }
            }
        }

        // Restore hyperlinks from scrollback Line into CellExtras.
        //
        // Total column-writes are bounded to O(cols): each cell holds at most
        // one hyperlink, so a legitimate (disjoint) line never hits the budget,
        // while a crafted stored Line (checkpoint restore, Kitty unscroll) can
        // carry up to u16::MAX overlapping full-width spans, which would
        // otherwise cost O(spans * cols) under the terminal lock. Only crafted
        // overlap is truncated. Both passes walk the spans with the same
        // budget, so they cover the same columns without a column list.
        if let Some(spans) = line.hyperlinks() {
            let mut any_hyperlink = false;
            let mut budget = cols;
            'fill: for span in spans {
                for hcol in filled_span_cols(span.start_col, span.end_col, filled) {
                    if budget == 0 {
                        break 'fill;
                    }
                    let extra = self
                        .storage
                        .extras
                        .get_or_create(CellCoord::new(row_idx, hcol));
                    extra.set_hyperlink(Some(span.url.clone()));
                    extra.set_hyperlink_id(span.id.clone());
                    any_hyperlink = true;
                    budget -= 1;
                }
            }
            // Set HAS_EXTRAS flags on exactly the cells loop 1 wrote (same span order,
            // same budget => same columns).
            if any_hyperlink {
                if let Some(idx) = self.storage.row_index(row_idx)
                    && let Some(r) = self.storage.rows.get_mut(idx)
                {
                    let mut budget = cols;
                    'flag: for span in spans {
                        for hcol in filled_span_cols(span.start_col, span.end_col, filled) {
                            if budget == 0 {
                                break 'flag;
                            }
                            if let Some(cell) = r.get_mut(hcol) {
                                cell.set_has_extras(true);
                            }
                            budget -= 1;
                        }
                    }
                }
                // Defense-in-depth: enforce hyperlink limit after restoring from
                // scrollback, in case materialized lines carry accumulated spam.
                self.storage.extras.enforce_hyperlink_limit();
            }
        }

        // Restore SGR 58 underline colours from scrollback Line into CellExtras,
        // with the same O(cols) budget as the hyperlink restore.
        // `set_underline_color_u32` re-derives RGB/indexed so an indexed colour
        // resolves against the live palette. Both passes (extra, then flag) use
        // the same budget and so cover the same columns.
        if let Some(spans) = line.underline_colors() {
            let mut any_ul = false;
            let mut budget = cols;
            'ul_fill: for span in spans {
                for ucol in filled_span_cols(span.start_col, span.end_col, filled) {
                    if budget == 0 {
                        break 'ul_fill;
                    }
                    self.storage
                        .extras
                        .get_or_create(CellCoord::new(row_idx, ucol))
                        .set_underline_color_u32(Some(span.color));
                    any_ul = true;
                    budget -= 1;
                }
            }
            if any_ul
                && let Some(idx) = self.storage.row_index(row_idx)
                && let Some(r) = self.storage.rows.get_mut(idx)
            {
                let mut budget = cols;
                'ul_flag: for span in spans {
                    for ucol in filled_span_cols(span.start_col, span.end_col, filled) {
                        if budget == 0 {
                            break 'ul_flag;
                        }
                        if let Some(cell) = r.get_mut(ucol) {
                            cell.set_has_extras(true);
                        }
                        budget -= 1;
                    }
                }
            }
        }

        // Restore inline images into CellExtras in the one-ref-per-cell form a
        // live placement has, so an unscrolled picture takes the same draw path.
        // Layers are stacked in draw order under the same crafted-input budget
        // (O(cols) per layer a cell can hold); the payload `Arc` is cloned,
        // never the raster.
        if let Some(spans) = line.images() {
            let mut any_image = false;
            let mut budget = usize::from(cols) * crate::extra::MAX_IMAGE_LAYERS;
            'img_fill: for span in spans {
                for icol in span.start_col..span.end_col.min(cols) {
                    if budget == 0 {
                        break 'img_fill;
                    }
                    if let Some(image) = super::scroll_materialize::span_layer_at(span, icol) {
                        self.storage
                            .extras
                            .stack_image(CellCoord::new(row_idx, icol), image);
                        any_image = true;
                    }
                    budget -= 1;
                }
            }
            if any_image
                && let Some(idx) = self.storage.row_index(row_idx)
                && let Some(r) = self.storage.rows.get_mut(idx)
            {
                let mut budget = cols;
                'img_flag: for span in spans {
                    for icol in span.start_col..span.end_col.min(cols) {
                        if budget == 0 {
                            break 'img_flag;
                        }
                        if let Some(cell) = r.get_mut(icol) {
                            cell.set_has_extras(true);
                        }
                        budget -= 1;
                    }
                }
            }
        }

        // LAST: mirror each wide lead's rendition-valued extras onto its
        // continuation spacer, reading what every restore above just wrote.
        self.mirror_wide_spacer_extras(row_idx);
    }

    /// Give every wide lead's continuation spacer the lead's rendition-valued
    /// extras: the deferred-fill counterpart of `scroll_materialize`'s
    /// `mirror_wide_spacer_extras`, which explains the rule.
    ///
    /// `CellFlags::wide_continuation_of` gives the spacer the lead's rendition
    /// flags, which select the truecolor fg/bg and SGR 58 underline colour;
    /// `fill_row_cells` emits those for the lead's column only, so without this
    /// a checkpoint restore, Kitty unscroll or reflow pullback would render the
    /// character's right half differently from the materializer.
    ///
    /// Two passes over at most `cols` columns: collect `(lead, spacer)` pairs
    /// while the row is borrowed, then write while the extras are.
    /// `cell_extra_mut` sets HAS_EXTRAS as it creates the entry; the live
    /// render path gates its map probe on that bit.
    fn mirror_wide_spacer_extras(&mut self, row_idx: u16) {
        let Some(idx) = self.storage.row_index(row_idx) else {
            return;
        };
        let Some(r) = self.storage.rows.get(idx) else {
            return;
        };
        // The SAME predicate the scroll-off extractor uses to DROP a spacer
        // column, so the columns healed here are exactly the columns it drops.
        let cells = r.as_slice();
        let mut pairs: aterm_alloc::SmallVec<(u16, u16), 4> = aterm_alloc::SmallVec::new();
        for i in 1..cells.len() {
            if !super::scroll_convert::is_spacer(cells, i) {
                continue;
            }
            let (Ok(lead), Ok(spacer)) = (u16::try_from(i - 1), u16::try_from(i)) else {
                break;
            };
            pairs.push((lead, spacer));
        }

        for (lead, spacer) in pairs {
            // Ring-aware reads: `fill_row_from_line` writes RGB into the map, but
            // a row it fills can already hold ring entries, and these are the
            // accessors the extractor itself reads through.
            let fg = self.storage.extras.fg_rgb_for(row_idx, lead);
            let bg = self.storage.extras.bg_rgb_for(row_idx, lead);
            let underline = self
                .storage
                .extras
                .get(CellCoord::new(row_idx, lead))
                .and_then(crate::CellExtra::underline_color_u32);
            if fg.is_none() && bg.is_none() && underline.is_none() {
                continue;
            }
            // Copy only where the spacer has nothing of its own, so a span that
            // already covers the pair keeps its value and the pass is idempotent.
            let have_fg = self.storage.extras.fg_rgb_for(row_idx, spacer).is_some();
            let have_bg = self.storage.extras.bg_rgb_for(row_idx, spacer).is_some();
            let have_underline = self
                .storage
                .extras
                .get(CellCoord::new(row_idx, spacer))
                .and_then(crate::CellExtra::underline_color_u32)
                .is_some();
            let dst = self.storage.cell_extra_mut(row_idx, spacer);
            if fg.is_some() && !have_fg {
                dst.set_fg_rgb(fg);
            }
            if bg.is_some() && !have_bg {
                dst.set_bg_rgb(bg);
            }
            if underline.is_some() && !have_underline {
                dst.set_underline_color_u32(underline);
            }
        }
    }
}

/// Deferred extra to apply after the row borrow is released.
enum DeferredExtra {
    /// BMP base + combining marks: attach marks via `add_combining`.
    /// SmallVec avoids heap allocation for the typical 1-3 combining mark case.
    CombiningMarks(u16, aterm_alloc::SmallVec<char, 4>),
    /// Non-BMP / ZWJ sequence: store as complex char string.
    ComplexChar(u16, Arc<str>),
    /// RGB foreground color from scrollback Line attrs.
    RgbFg(u16, [u8; 3]),
    /// RGB background color from scrollback Line attrs.
    RgbBg(u16, [u8; 3]),
}

/// Phase 1: Populate row cells from scrollback Line text, returning deferred
/// extras and the column the fill reached: the line's cells are the first
/// `filled`, and none past them holds anything of it ([`filled_span_cols`]).
///
/// Iterates grapheme units (via [`advance_grapheme_unit`]) and classifies each as:
/// - Simple BMP → normal cell
/// - BMP + combining marks → normal cell + deferred combining extras
/// - Non-BMP / ZWJ → complex cell + deferred complex char extras
fn fill_row_cells(row: &mut Row, text: &str, line: &Line, cols: u16) -> (Vec<DeferredExtra>, u16) {
    use super::scroll_materialize::advance_grapheme_unit_wide;
    use crate::{CellFlags, PackedColor};

    let mut deferred = Vec::new();
    // `unit_char_start` is monotone across this walk, so a run cursor reads the
    // RLE attrs in O(runs) total instead of rescanning per cell. This filler
    // runs once per rewrapped ring row on a width change.
    let mut attr_cursor = line.attr_cursor();
    let mut byte_idx: usize = 0;
    let mut char_idx: usize = 0;
    let mut col: u16 = 0;

    while byte_idx < text.len() && col < cols {
        let c = super::scroll_materialize::char_at(text, byte_idx);
        if super::scroll_materialize::is_zero_width(c) {
            byte_idx += c.len_utf8();
            char_idx += 1;
            continue;
        }

        let unit_byte_start = byte_idx;
        let unit_char_start = char_idx;
        // Effective width, replaying the live VS16/VS15 transitions (see
        // `advance_grapheme_unit_wide`) so a restored `❤️` keeps two columns,
        // with the live writer's row-edge exception (a VS16 widening fails at
        // the last column, so demote there).
        let unit = advance_grapheme_unit_wide(text, &mut byte_idx);
        let chars_consumed = unit.chars;
        let attrs = attr_cursor.attr_at(unit_char_start);
        let is_wide = super::scroll_materialize::stored_unit_is_wide(unit, attrs)
            && !(unit.vs16_widened && col + 1 >= cols);
        char_idx += chars_consumed;
        let unit_str = &text[unit_byte_start..byte_idx];
        let style = LineCellStyle {
            fg: PackedColor(attrs.fg),
            bg: PackedColor(attrs.bg),
            flags: CellFlags::from_bits(attrs.flags),
        };

        let (new_col, extra) =
            place_line_cell(row, col, cols, unit_str, chars_consumed, &style, is_wide);
        // No column progress: a wide char could not fit at the last column, so
        // the row is full. Stop rather than scanning the rest of a possibly
        // oversized (injected/restored) line under the lock. A legitimate row
        // never starts a wide char it cannot fit, and `extra` is always None on
        // the can't-fit branches, so nothing is lost.
        if new_col == col {
            break;
        }
        if let Some(e) = extra {
            deferred.push(e);
        }
        // Restore RGB colors from scrollback Line attrs into CellExtras.
        if new_col > col {
            if style.fg.is_rgb() {
                let (r, g, b) = style.fg.rgb_components();
                deferred.push(DeferredExtra::RgbFg(col, [r, g, b]));
            }
            if style.bg.is_rgb() {
                let (r, g, b) = style.bg.rgb_components();
                deferred.push(DeferredExtra::RgbBg(col, [r, g, b]));
            }
        }
        col = new_col;
    }
    // The line is content, so every cell it filled is: a printed blank (a
    // space at a rewrap chunk end, trailing blanks of a row its successor
    // continues) stays inside the row's length, as the write path counts a
    // printed space. `Row::set` alone counts only non-empty cells, and a ring
    // row that later spills to the tiered store is staged at its length.
    row.update_len(col);
    // A line ends where its content ends. One that fills all but the last
    // cell left that cell as the rewrap's early-wrap hole (the next glyph was
    // wide and did not fit); record it, so a rewrap that rejoins this row with
    // its continuation drops that cell and nothing else. On a row nothing
    // continues the mark changes nothing: the rejoining readers consult it
    // only for a continued row, and the history staging that consults it on
    // every row (`DeferredLine::new_boxed`) cuts the last cell, which this
    // line never filled (unless a background-coloured erase blanks it since).
    row.set_early_wrap_hole(usize::from(col) + 1 == row.cells_len());
    (deferred, col)
}

/// Bundled style for a cell being placed from a scrollback Line.
struct LineCellStyle {
    fg: crate::PackedColor,
    bg: crate::PackedColor,
    flags: crate::CellFlags,
}

/// Classify and place a grapheme unit into the row.
///
/// `unit_str` is borrowed from the source text; `chars_consumed` is its
/// character count. The base character is derived from `unit_str`.
///
/// Returns `(new_column, optional_deferred_extra)`.
fn place_line_cell(
    row: &mut Row,
    col: u16,
    cols: u16,
    unit_str: &str,
    chars_consumed: usize,
    style: &LineCellStyle,
    is_wide: bool,
) -> (u16, Option<DeferredExtra>) {
    use crate::Cell;

    let c = super::scroll_materialize::char_at(unit_str, 0);
    let base_is_bmp = (c as u32) <= Cell::MAX_DIRECT_CODEPOINT;

    // BMP base + only combining marks → normal cell + deferred combining.
    let combining_only = base_is_bmp
        && chars_consumed > 1
        && unit_str[c.len_utf8()..]
            .chars()
            .all(|ch| aterm_grapheme::char_width(ch) == 0);

    if chars_consumed == 1 && base_is_bmp {
        (set_cell(row, col, cols, c, style, is_wide), None)
    } else if combining_only {
        let new_col = set_cell(row, col, cols, c, style, is_wide);
        let extra = if new_col > col {
            let marks: aterm_alloc::SmallVec<char, 4> = unit_str[c.len_utf8()..].chars().collect();
            Some(DeferredExtra::CombiningMarks(col, marks))
        } else {
            None
        };
        (new_col, extra)
    } else {
        set_complex_cell(row, col, cols, unit_str, style, is_wide)
    }
}

/// Set a normal (non-complex) cell, handling wide chars.
fn set_cell(row: &mut Row, col: u16, cols: u16, c: char, s: &LineCellStyle, is_wide: bool) -> u16 {
    use crate::{Cell, CellFlags};
    let flags = if is_wide {
        s.flags.union(CellFlags::WIDE)
    } else {
        s.flags.difference(CellFlags::WIDE)
    };
    if is_wide && col + 1 < cols {
        row.set(col, Cell::with_style(c, s.fg, s.bg, flags));
        // The spacer inherits the lead's rendition (see `wide_continuation_of`);
        // a bare role bit would un-highlight wide glyphs on scroll-back.
        row.set(
            col + 1,
            Cell::with_style(' ', s.fg, s.bg, flags.wide_continuation_of()),
        );
        col.saturating_add(2)
    } else if is_wide {
        col // wide at last column — can't fit
    } else {
        row.set(col, Cell::with_style(c, s.fg, s.bg, flags));
        col.saturating_add(1)
    }
}

/// Set a complex cell (non-BMP, ZWJ sequence, etc.) with overflow index.
///
/// `Arc::from(unit_str)` builds the stored string directly from the borrowed
/// slice.
fn set_complex_cell(
    row: &mut Row,
    col: u16,
    cols: u16,
    unit_str: &str,
    s: &LineCellStyle,
    is_wide: bool,
) -> (u16, Option<DeferredExtra>) {
    use crate::{Cell, CellFlags};
    let flags = if is_wide {
        s.flags.union(CellFlags::WIDE)
    } else {
        s.flags.difference(CellFlags::WIDE)
    };
    let mut cell = Cell::with_style(' ', s.fg, s.bg, flags);
    cell.set_overflow_index(0);

    if is_wide && col + 1 < cols {
        row.set(col, cell);
        row.set(
            col + 1,
            Cell::with_style(' ', s.fg, s.bg, flags.wide_continuation_of()),
        );
        (
            col.saturating_add(2),
            Some(DeferredExtra::ComplexChar(col, Arc::from(unit_str))),
        )
    } else if is_wide {
        (col, None)
    } else {
        row.set(col, cell);
        (
            col.saturating_add(1),
            Some(DeferredExtra::ComplexChar(col, Arc::from(unit_str))),
        )
    }
}

/// Apply deferred extras (combining marks, complex chars, RGB colors) to CellExtras.
fn apply_deferred_extras(extras: &mut CellExtras, row_idx: u16, deferred: Vec<DeferredExtra>) {
    for extra in deferred {
        match extra {
            DeferredExtra::CombiningMarks(col, marks) => {
                let entry = extras.get_or_create(CellCoord::new(row_idx, col));
                for mark in marks {
                    entry.add_combining(mark);
                }
            }
            DeferredExtra::ComplexChar(col, s) => {
                extras
                    .get_or_create(CellCoord::new(row_idx, col))
                    .set_complex_char(Some(s));
            }
            DeferredExtra::RgbFg(col, rgb) => {
                extras
                    .get_or_create(CellCoord::new(row_idx, col))
                    .set_fg_rgb(Some(rgb));
            }
            DeferredExtra::RgbBg(col, rgb) => {
                extras
                    .get_or_create(CellCoord::new(row_idx, col))
                    .set_bg_rgb(Some(rgb));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_scrollback::{CellAttrs, Line, Rle};

    // =========================================================================
    // fill_row_from_line — basic text
    // =========================================================================

    #[test]
    fn test_fill_row_from_empty_line() {
        let mut grid = Grid::new(3, 10);
        let line = Line::new();
        grid.fill_row_from_line(0, &line, 10);

        let row = grid.row(0).expect("row 0 should exist");
        assert!(
            row.is_empty(),
            "filling from empty line should leave row empty"
        );
    }

    #[test]
    fn test_fill_row_from_ascii_line() {
        let mut grid = Grid::new(3, 20);
        let line = Line::from_bytes(b"Hello");
        grid.fill_row_from_line(0, &line, 20);

        let row = grid.row(0).expect("row 0 should exist");
        for (i, expected) in "Hello".chars().enumerate() {
            assert_eq!(
                row.get(i as u16).unwrap().char(),
                expected,
                "char mismatch at col {i}"
            );
        }
        // Remaining cells should be space (cleared)
        for col in 5..20u16 {
            assert_eq!(
                row.get(col).unwrap().char(),
                ' ',
                "col {col} should be space after text"
            );
        }
    }

    #[test]
    fn test_fill_row_from_line_preserves_wrapped_flag() {
        let mut grid = Grid::new(3, 20);
        let mut line = Line::from_bytes(b"wrapped");
        line.set_wrapped(true);
        grid.fill_row_from_line(0, &line, 20);

        let row = grid.row(0).expect("row 0 should exist");
        assert!(
            row.is_wrapped(),
            "wrapped flag should be preserved from scrollback Line"
        );
    }

    #[test]
    fn test_fill_row_from_line_not_wrapped() {
        let mut grid = Grid::new(3, 20);
        let line = Line::from_bytes(b"not wrapped");
        grid.fill_row_from_line(0, &line, 20);

        let row = grid.row(0).expect("row 0 should exist");
        assert!(
            !row.is_wrapped(),
            "non-wrapped line should not set wrapped flag"
        );
    }

    #[test]
    fn test_fill_row_truncates_to_cols() {
        let mut grid = Grid::new(3, 5);
        let line = Line::from_bytes(b"LongTextThatExceedsColumns");
        grid.fill_row_from_line(0, &line, 5);

        // Only the first 5 characters should be placed
        let row = grid.row(0).expect("row 0 should exist");
        for (i, expected) in "LongT".chars().enumerate() {
            assert_eq!(
                row.get(i as u16).unwrap().char(),
                expected,
                "char mismatch at col {i} when truncating"
            );
        }
    }

    #[test]
    fn test_fill_row_from_line_clears_previous_content() {
        let mut grid = Grid::new(3, 10);

        // Write some content first
        grid.write_char('X');

        // Now fill from a different line
        let line = Line::from_bytes(b"AB");
        grid.fill_row_from_line(0, &line, 10);

        let row = grid.row(0).expect("row 0 should exist");
        assert_eq!(row.get(0).unwrap().char(), 'A');
        assert_eq!(row.get(1).unwrap().char(), 'B');
        // The old 'X' should be gone (row was cleared)
    }

    #[test]
    fn fill_row_hyperlinks_disjoint_are_correct_and_overlap_is_bounded() {
        use aterm_scrollback::HyperlinkSpan;
        let cols = 4u16;

        // LEGIT (disjoint) spans: the budget (== cols) never truncates, so each cell
        // gets its own span's URL.
        let mut grid = Grid::new(1, cols);
        let disjoint = vec![
            HyperlinkSpan::new(0, 2, Arc::from("a")),
            HyperlinkSpan::new(2, 4, Arc::from("b")),
        ];
        let line = Line::with_hyperlinks("wxyz", Rle::<CellAttrs>::new(), disjoint);
        grid.fill_row_from_line(0, &line, cols);
        let url = |g: &Grid, c: u16| {
            g.extras()
                .get(CellCoord::new(0, c))
                .and_then(|e| e.hyperlink())
                .map(|u| u.to_string())
        };
        assert_eq!(url(&grid, 0).as_deref(), Some("a"));
        assert_eq!(url(&grid, 1).as_deref(), Some("a"));
        assert_eq!(url(&grid, 2).as_deref(), Some("b"));
        assert_eq!(url(&grid, 3).as_deref(), Some("b"));

        // Crafted overlapping spans: the budget caps total column-writes at
        // cols, so the first full-width span fills every column and the rest are
        // skipped; every cell carries "first", showing the work is O(cols).
        let mut grid2 = Grid::new(1, cols);
        let overlap = vec![
            HyperlinkSpan::new(0, cols, Arc::from("first")),
            HyperlinkSpan::new(0, cols, Arc::from("second")),
            HyperlinkSpan::new(0, cols, Arc::from("third")),
        ];
        let line2 = Line::with_hyperlinks("wxyz", Rle::<CellAttrs>::new(), overlap);
        grid2.fill_row_from_line(0, &line2, cols);
        for c in 0..cols {
            assert_eq!(
                url(&grid2, c).as_deref(),
                Some("first"),
                "overlapping spans must stop after the budget (cols) is spent at col {c}"
            );
        }
    }

    /// A glyph the row cannot hold takes its link and underline colour with it,
    /// in both fills (`filled_span_cols`): a wide unit in a one-column row (the
    /// line a one-column rewrap gives it) and a wide unit at the last column.
    /// The cells before it keep theirs.
    #[test]
    fn a_glyph_the_row_cannot_hold_takes_its_link_and_underline_colour() {
        use aterm_scrollback::{HyperlinkSpan, UnderlineColorSpan};
        // (text, row width, the span's end, columns that hold a glyph)
        for (text, cols, end, held) in [("\u{1F680}", 1u16, 2u16, 0u16), ("a\u{4E2D}", 2, 3, 1)] {
            let mut line = Line::with_hyperlinks(
                text,
                Rle::<CellAttrs>::new(),
                vec![HyperlinkSpan::new(
                    0,
                    end,
                    Arc::from("https://example.com/"),
                )],
            );
            line.set_underline_colors(vec![UnderlineColorSpan::new(0, end, 0x0100_0001)]);

            let mut grid = Grid::new(1, cols);
            grid.fill_row_from_line(0, &line, cols);
            for col in 0..cols {
                let extra = grid.extras().get(CellCoord::new(0, col));
                let linked = extra.is_some_and(|e| e.hyperlink().is_some());
                let coloured = extra.is_some_and(|e| e.underline_color_u32().is_some());
                let claimed = grid.cell(0, col).is_some_and(crate::Cell::has_extras);
                let holds = col < held;
                assert_eq!(
                    (linked, coloured, claimed),
                    (holds, holds, holds),
                    "fill_row_from_line {text:?} at {cols} columns, column {col}"
                );
            }

            let mut pages = crate::PageStore::new();
            // SAFETY: `pages` outlives `row`, both dropped at the end of the loop body.
            let mut row = unsafe { Row::new(cols, &mut pages) };
            let extras = fill_row_into(&mut row, &line, cols, grid.styles());
            let links: Vec<_> = extras
                .hyperlinks
                .iter()
                .map(|s| (s.start_col, s.end_col))
                .collect();
            let colours: Vec<u16> = extras.underline_colors.iter().map(|&(c, _)| c).collect();
            let want_links = if held > 0 { vec![(0, held)] } else { vec![] };
            assert_eq!(links, want_links, "fill_row_into {text:?} links");
            assert_eq!(
                colours,
                (0..held).collect::<Vec<_>>(),
                "fill_row_into {text:?} underline colours"
            );
        }
    }

    #[test]
    fn test_fill_row_different_row_index() {
        let mut grid = Grid::new(5, 10);
        let line = Line::from_bytes(b"Row2");
        grid.fill_row_from_line(2, &line, 10);

        // Row 2 should have content
        let row = grid.row(2).expect("row 2 should exist");
        assert_eq!(row.get(0).unwrap().char(), 'R');
        assert_eq!(row.get(3).unwrap().char(), '2');

        // Row 0 should still be blank
        let row0 = grid.row(0).expect("row 0 should exist");
        assert_eq!(row0.get(0).unwrap().char(), ' ');
    }

    #[test]
    fn test_fill_row_out_of_bounds_row_is_noop() {
        let mut grid = Grid::new(3, 10);
        let line = Line::from_bytes(b"test");
        // Row 99 doesn't exist; should not panic
        grid.fill_row_from_line(99, &line, 10);

        // Existing rows should be unaffected
        let row = grid.row(0).expect("row 0 should exist");
        assert_eq!(row.get(0).unwrap().char(), ' ');
    }

    // =========================================================================
    // fill_row_from_line — styled content
    // =========================================================================

    #[test]
    fn test_fill_row_from_line_with_default_attrs() {
        let mut grid = Grid::new(3, 20);
        let attrs = Rle::with_value(CellAttrs::DEFAULT, 5);
        let line = Line::with_attrs("Hello", attrs);
        grid.fill_row_from_line(0, &line, 20);

        let row = grid.row(0).expect("row 0 should exist");
        for (i, expected) in "Hello".chars().enumerate() {
            assert_eq!(
                row.get(i as u16).unwrap().char(),
                expected,
                "char mismatch at col {i} with default attrs"
            );
        }
    }

    // =========================================================================
    // Scroll behavioral tests — scroll_up
    // =========================================================================

    #[test]
    fn test_scroll_up_content_shifts_up() {
        let mut grid = Grid::new(3, 10);
        // Write A on row 0, B on row 1, C on row 2
        grid.set_cursor(0, 0);
        grid.write_char('A');
        grid.set_cursor(1, 0);
        grid.write_char('B');
        grid.set_cursor(2, 0);
        grid.write_char('C');

        grid.scroll_up(1);

        // After scroll_up(1): row 0 = B, row 1 = C, row 2 = blank
        assert_eq!(grid.cell(0, 0).unwrap().char(), 'B');
        assert_eq!(grid.cell(1, 0).unwrap().char(), 'C');
        assert_eq!(grid.cell(2, 0).unwrap().char(), ' ');
    }

    #[test]
    fn test_scroll_down_content_shifts_down() {
        let mut grid = Grid::new(3, 10);
        grid.set_cursor(0, 0);
        grid.write_char('A');
        grid.set_cursor(1, 0);
        grid.write_char('B');
        grid.set_cursor(2, 0);
        grid.write_char('C');

        grid.scroll_down(1);

        // After scroll_down(1): row 0 = blank, row 1 = A, row 2 = B
        assert_eq!(grid.cell(0, 0).unwrap().char(), ' ');
        assert_eq!(grid.cell(1, 0).unwrap().char(), 'A');
        assert_eq!(grid.cell(2, 0).unwrap().char(), 'B');
    }

    // =========================================================================
    // Scroll within scroll region (DECSTBM)
    // =========================================================================

    #[test]
    fn test_scroll_region_up_within_region() {
        let mut grid = Grid::new(5, 10);
        // Write A-E on rows 0-4
        for row in 0..5u16 {
            grid.set_cursor(row, 0);
            grid.write_char((b'A' + row as u8) as char);
        }

        // Set scroll region: rows 1-3
        grid.set_scroll_region(1, 3);
        grid.scroll_region_up(1);

        // Row 0 unchanged (outside region)
        assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
        // Region rows 1-3: B shifted out, C->1, D->2, blank->3
        assert_eq!(grid.cell(1, 0).unwrap().char(), 'C');
        assert_eq!(grid.cell(2, 0).unwrap().char(), 'D');
        assert_eq!(grid.cell(3, 0).unwrap().char(), ' ');
        // Row 4 unchanged (outside region)
        assert_eq!(grid.cell(4, 0).unwrap().char(), 'E');
    }

    #[test]
    fn test_scroll_region_down_within_region() {
        let mut grid = Grid::new(5, 10);
        for row in 0..5u16 {
            grid.set_cursor(row, 0);
            grid.write_char((b'A' + row as u8) as char);
        }

        // Set scroll region: rows 1-3
        grid.set_scroll_region(1, 3);
        grid.scroll_region_down(1);

        // Row 0 unchanged
        assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
        // Region: blank at top, B->2, C->3, D pushed out
        assert_eq!(grid.cell(1, 0).unwrap().char(), ' ');
        assert_eq!(grid.cell(2, 0).unwrap().char(), 'B');
        assert_eq!(grid.cell(3, 0).unwrap().char(), 'C');
        // Row 4 unchanged
        assert_eq!(grid.cell(4, 0).unwrap().char(), 'E');
    }

    // =========================================================================
    // Multiple scroll operations
    // =========================================================================

    #[test]
    fn test_multiple_scroll_up() {
        let mut grid = Grid::new(4, 10);
        for row in 0..4u16 {
            grid.set_cursor(row, 0);
            grid.write_char((b'A' + row as u8) as char);
        }

        grid.scroll_up(2);

        // After scrolling up by 2: C->0, D->1, blank->2, blank->3
        assert_eq!(grid.cell(0, 0).unwrap().char(), 'C');
        assert_eq!(grid.cell(1, 0).unwrap().char(), 'D');
        assert_eq!(grid.cell(2, 0).unwrap().char(), ' ');
        assert_eq!(grid.cell(3, 0).unwrap().char(), ' ');
    }

    #[test]
    fn test_multiple_scroll_down() {
        let mut grid = Grid::new(4, 10);
        for row in 0..4u16 {
            grid.set_cursor(row, 0);
            grid.write_char((b'A' + row as u8) as char);
        }

        grid.scroll_down(2);

        // After scrolling down by 2: blank->0, blank->1, A->2, B->3
        assert_eq!(grid.cell(0, 0).unwrap().char(), ' ');
        assert_eq!(grid.cell(1, 0).unwrap().char(), ' ');
        assert_eq!(grid.cell(2, 0).unwrap().char(), 'A');
        assert_eq!(grid.cell(3, 0).unwrap().char(), 'B');
    }

    // =========================================================================
    // Edge cases
    // =========================================================================

    #[test]
    fn test_scroll_up_count_exceeds_region() {
        let mut grid = Grid::new(3, 10);
        for row in 0..3u16 {
            grid.set_cursor(row, 0);
            grid.write_char((b'A' + row as u8) as char);
        }

        // Scroll up by 100 in a 3-row region
        grid.set_scroll_region(0, 2);
        grid.scroll_region_up(100);

        // All rows should be blank (scrolled everything out)
        for row in 0..3u16 {
            assert_eq!(
                grid.cell(row, 0).unwrap().char(),
                ' ',
                "row {row} should be blank after excessive scroll"
            );
        }
    }

    #[test]
    fn test_scroll_down_count_exceeds_region() {
        let mut grid = Grid::new(3, 10);
        for row in 0..3u16 {
            grid.set_cursor(row, 0);
            grid.write_char((b'A' + row as u8) as char);
        }

        grid.set_scroll_region(0, 2);
        grid.scroll_region_down(100);

        // All rows should be blank
        for row in 0..3u16 {
            assert_eq!(
                grid.cell(row, 0).unwrap().char(),
                ' ',
                "row {row} should be blank after excessive scroll down"
            );
        }
    }

    #[test]
    fn test_scroll_up_zero_is_noop() {
        let mut grid = Grid::new(3, 10);
        grid.set_cursor(0, 0);
        grid.write_char('A');

        grid.scroll_up(0);

        assert_eq!(grid.cell(0, 0).unwrap().char(), 'A');
    }

    #[test]
    fn test_scroll_region_full_screen_delegates_to_scroll_up() {
        let mut grid = Grid::new(3, 10);
        for row in 0..3u16 {
            grid.set_cursor(row, 0);
            grid.write_char((b'A' + row as u8) as char);
        }

        // Full-screen region scroll should act like regular scroll_up
        grid.set_scroll_region(0, 2);
        grid.scroll_region_up(1);

        assert_eq!(grid.cell(0, 0).unwrap().char(), 'B');
        assert_eq!(grid.cell(1, 0).unwrap().char(), 'C');
        assert_eq!(grid.cell(2, 0).unwrap().char(), ' ');
    }
}
