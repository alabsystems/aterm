// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Populates grid rows from scrollback [`Line`]s while preserving grapheme clusters and extras.

use std::sync::Arc;

use aterm_scrollback::Line;

use super::Grid;
use super::ScrolledRowExtras;
use super::row_u16;
use super::scroll_materialize::filled_span_cols;
use crate::Row;
use crate::{Cell, CellCoord, CellExtras};

/// Fills a detached [`Row`] from `line` and returns its [`ScrolledRowExtras`].
pub(crate) fn fill_row_into(row: &mut Row, line: &Line, cols: u16) -> ScrolledRowExtras {
    debug_assert_eq!(row.len(), 0, "fill_row_into takes a fresh row");
    let mut extras = ScrolledRowExtras::default();
    let Some(text) = line.as_str() else {
        row.set_wrapped(line.is_wrapped());
        return extras;
    };
    let filled = fill_row_cells(row, text, line, cols, &mut |_, extra| match extra {
        RestoredExtra::CombiningMarks(col, marks) => {
            extras.combining.push((col, marks.chars().collect()));
        }
        RestoredExtra::ComplexChar(col, s) => extras.complex_chars.push((col, Arc::from(s))),
        RestoredExtra::NonBmpChar(col, ch) => {
            let mut encoded = [0; 4];
            extras
                .complex_chars
                .push((col, Arc::from(&*ch.encode_utf8(&mut encoded))));
        }
        RestoredExtra::RgbFg(col, rgb) => extras.rgb_fg.push((col, rgb)),
        RestoredExtra::RgbBg(col, rgb) => extras.rgb_bg.push((col, rgb)),
    });
    row.set_wrapped(line.is_wrapped());
    if let Some(spans) = line.hyperlinks() {
        for span in spans {
            let held = filled_span_cols(span.start_col, span.end_col, filled);
            if held.is_empty() {
                continue;
            }
            extras
                .hyperlinks
                .push(span.with_range(held.start, held.end));
        }
    }
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

/// Returns the wide glyph opening `row` when it continues the previous row.
pub(super) fn wrap_opening(row: &Row) -> Option<Cell> {
    row.get(0)
        .filter(|glyph| row.is_wrapped() && glyph.is_wide())
        .copied()
}

impl Grid {
    /// Paints early-wrap holes at the end of visible rows in `rows`.
    pub(super) fn paint_wrap_holes(&mut self, rows: core::ops::Range<u16>) {
        for row in rows {
            let Some(glyph) = self
                .storage
                .row_at_screen(row.saturating_add(1))
                .and_then(wrap_opening)
            else {
                continue;
            };
            let mut blank = Cell::bce_blank(glyph.colors());
            let bg_rgb = if blank.colors().bg_is_rgb() {
                self.storage.extras.bg_rgb_for(row.saturating_add(1), 0)
            } else {
                None
            };
            blank.set_has_extras(bg_rgb.is_some());
            let Some(r) = self.row_mut(row).filter(|r| r.has_early_wrap_hole()) else {
                continue;
            };
            let col = row_u16(r.cells_len().saturating_sub(1));
            r.set(col, blank);
            r.set_early_wrap_hole(true);
            if let Some(rgb) = bg_rgb {
                self.storage
                    .extras
                    .get_or_create(CellCoord::new(row, col))
                    .set_bg_rgb(Some(rgb));
            }
        }
    }

    /// Fills visible row `row_idx` and its `CellExtras` from `line`.
    pub(crate) fn fill_row_from_line(&mut self, row_idx: u16, line: &Line, cols: u16) {
        if self.row(row_idx).is_none() {
            return;
        }
        self.storage.extras.clear_row_retaining_capacity(row_idx);
        self.fill_row_from_line_cleared(row_idx, line, cols);
        self.storage.extras.maybe_shrink();
    }

    /// Fill a row whose visible extras are clear; report restored owned text storage.
    pub(super) fn fill_row_from_line_cleared(
        &mut self,
        row_idx: u16,
        line: &Line,
        cols: u16,
    ) -> bool {
        let mut owned_text = false;
        let Some(text) = line.as_str() else {
            if let Some(row) = self.row_mut(row_idx) {
                row.clear();
                row.set_wrapped(line.is_wrapped());
            }
            return false;
        };

        let filled = {
            if row_idx >= self.storage.visible_rows {
                return false;
            }
            let Some(idx) = self.storage.row_index(row_idx) else {
                return false;
            };
            let visible_rows = self.storage.visible_rows;
            let Some(row) = self.storage.rows.get_mut(idx) else {
                return false;
            };
            let extras = &mut self.storage.cursor_state.extras;
            row.clear();
            let filled = fill_row_cells(row, text, line, cols, &mut |row, extra| {
                if let Some(cell) = row.get_mut(extra.col()) {
                    cell.set_has_extras(true);
                }
                apply_row_extra(extras, row_idx, visible_rows, cols, extra, &mut owned_text);
            });
            row.set_wrapped(line.is_wrapped());
            filled
        };

        if let Some(spans) = line.hyperlinks() {
            let mut any_hyperlink = false;
            let mut budget = cols;
            'fill: for span in spans {
                for hcol in filled_span_cols(span.start_col, span.end_col, filled) {
                    if budget == 0 {
                        break 'fill;
                    }
                    self.storage
                        .extras
                        .modify(CellCoord::new(row_idx, hcol), |extra| {
                            extra.set_hyperlink_data(Some(Arc::clone(&span.payload)));
                        });
                    any_hyperlink = true;
                    budget -= 1;
                }
            }
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
                self.storage.extras.enforce_hyperlink_limit();
            }
        }

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

        if let Some(spans) = line.images() {
            self.stamp_row_image_spans(row_idx, spans, cols);
        }

        self.mirror_wide_spacer_extras(row_idx);
        owned_text
    }

    pub(super) fn stamp_row_image_spans(
        &mut self,
        row_idx: u16,
        spans: &[aterm_scrollback::ImageSpan],
        cols: u16,
    ) {
        let mut any_image = false;
        let max_ops = usize::from(cols) * crate::extra::MAX_IMAGE_LAYERS;
        let mut budget = max_ops;
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
            let mut budget = max_ops;
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

    fn mirror_wide_spacer_extras(&mut self, row_idx: u16) {
        let Some(idx) = self.storage.row_index(row_idx) else {
            return;
        };
        let Some(r) = self.storage.rows.get(idx) else {
            return;
        };
        let len = r.as_slice().len();
        for i in 1..len {
            // Rendition mirroring does not change wide/spacer flags, so later
            // pairs can be read directly after each earlier pair is updated.
            if !super::scroll_convert::is_spacer(self.storage.rows[idx].as_slice(), i) {
                continue;
            }
            let (Ok(lead), Ok(spacer)) = (u16::try_from(i - 1), u16::try_from(i)) else {
                break;
            };
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
            let have_fg = self.storage.extras.fg_rgb_for(row_idx, spacer).is_some();
            let have_bg = self.storage.extras.bg_rgb_for(row_idx, spacer).is_some();
            let have_underline = self
                .storage
                .extras
                .get(CellCoord::new(row_idx, spacer))
                .and_then(crate::CellExtra::underline_color_u32)
                .is_some();
            if (fg.is_none() || have_fg)
                && (bg.is_none() || have_bg)
                && (underline.is_none() || have_underline)
            {
                continue;
            }
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

#[cfg_attr(test, derive(Debug, PartialEq))]
enum RestoredExtra<'a> {
    /// Combining marks on a BMP base, attached via `add_combining`.
    CombiningMarks(u16, &'a str),
    /// Multi-scalar grapheme: the consumer stores its owned complex string.
    ComplexChar(u16, &'a str),
    /// A single non-BMP scalar fits the existing dense character ring.
    NonBmpChar(u16, char),
    /// RGB foreground color from scrollback Line attrs.
    RgbFg(u16, [u8; 3]),
    /// RGB background color from scrollback Line attrs.
    RgbBg(u16, [u8; 3]),
}

impl RestoredExtra<'_> {
    const fn col(&self) -> u16 {
        match self {
            Self::CombiningMarks(col, _)
            | Self::ComplexChar(col, _)
            | Self::NonBmpChar(col, _)
            | Self::RgbFg(col, _)
            | Self::RgbBg(col, _) => *col,
        }
    }
}

/// Lay out cells, emitting each extra after its cell is written so the consumer
/// can mark that cell and store the payload without retaining a batch.
fn fill_row_cells<'a>(
    row: &mut Row,
    text: &'a str,
    line: &'a Line,
    cols: u16,
    sink: &mut impl FnMut(&mut Row, RestoredExtra<'a>),
) -> u16 {
    if let Some(filled) = fill_ascii_row_cells(row, text, line, cols, sink) {
        return filled;
    }
    fill_row_cells_general(row, text, line, cols, sink)
}

/// Printable ASCII with ordinary cell widths needs no grapheme walk. Restrict
/// the whole line to the row's width, so the test is bounded and a following
/// combining character cannot change the last cell we restore.
fn fill_ascii_row_cells<'a>(
    row: &mut Row,
    text: &'a str,
    line: &'a Line,
    cols: u16,
    sink: &mut impl FnMut(&mut Row, RestoredExtra<'a>),
) -> Option<u16> {
    use crate::{CellFlags, PackedColor, PackedColors};
    use aterm_scrollback::CellAttrs;

    if text.len() > usize::from(cols)
        || !text.bytes().all(|byte| (b' '..=b'~').contains(&byte))
        || line.attrs().is_some_and(|attrs| {
            let mut covered = 0usize;
            for run in attrs.runs() {
                if covered >= text.len() {
                    break;
                }
                if run.length != 0
                    && CellFlags::from_bits(run.value.flags).contains(CellFlags::WIDE)
                {
                    return true;
                }
                covered = covered.saturating_add(run.length as usize);
            }
            false
        })
    {
        return None;
    }

    let mut cursor = line.attr_cursor();
    let mut current = CellAttrs::DEFAULT;
    let mut colors = PackedColors::DEFAULT;
    let mut flags = CellFlags::empty();
    let (mut fg_rgb, mut bg_rgb) = (None, None);
    for (index, byte) in text.bytes().enumerate() {
        let attrs = cursor.attr_at(index).with_opens_cell(false);
        if attrs != current {
            current = attrs;
            let (fg, bg) = (PackedColor(attrs.fg), PackedColor(attrs.bg));
            colors = Cell::convert_colors(fg, bg);
            flags = CellFlags::from_bits(attrs.flags);
            fg_rgb = fg.is_rgb().then(|| {
                let (r, g, b) = fg.rgb_components();
                [r, g, b]
            });
            bg_rgb = bg.is_rgb().then(|| {
                let (r, g, b) = bg.rgb_components();
                [r, g, b]
            });
        }
        let col = row_u16(index);
        row.set(col, Cell::from_ascii_styled(byte, colors, flags));
        if let Some(rgb) = fg_rgb {
            sink(row, RestoredExtra::RgbFg(col, rgb));
        }
        if let Some(rgb) = bg_rgb {
            sink(row, RestoredExtra::RgbBg(col, rgb));
        }
    }
    let col = row_u16(text.len());
    finish_row_cells(row, col, cols);
    Some(col)
}

fn fill_row_cells_general<'a>(
    row: &mut Row,
    text: &'a str,
    line: &'a Line,
    cols: u16,
    sink: &mut impl FnMut(&mut Row, RestoredExtra<'a>),
) -> u16 {
    use crate::{CellFlags, PackedColor};

    let mut col: u16 = 0;
    for cell in super::scroll_materialize::StoredCells::new(line, text, cols) {
        let style = LineCellStyle {
            fg: PackedColor(cell.attrs.fg),
            bg: PackedColor(cell.attrs.bg),
            flags: CellFlags::from_bits(cell.attrs.flags),
        };

        let (new_col, extra) = place_line_cell(
            row, cell.col, cols, cell.text, cell.chars, &style, cell.wide,
        );
        debug_assert!(new_col > cell.col, "the walk lays out only cells that fit");
        if let Some(e) = extra {
            sink(row, e);
        }
        if style.fg.is_rgb() {
            let (r, g, b) = style.fg.rgb_components();
            sink(row, RestoredExtra::RgbFg(cell.col, [r, g, b]));
        }
        if style.bg.is_rgb() {
            let (r, g, b) = style.bg.rgb_components();
            sink(row, RestoredExtra::RgbBg(cell.col, [r, g, b]));
        }
        col = new_col;
    }
    finish_row_cells(row, col, cols);
    col
}

fn finish_row_cells(row: &mut Row, col: u16, cols: u16) {
    row.update_len(col);
    row.set_early_wrap_hole(super::scroll_materialize::leaves_wrap_hole(col, cols));
    row.set_ends_at_len(u32::from(col) + 1 < u32::from(cols));
}

struct LineCellStyle {
    fg: crate::PackedColor,
    bg: crate::PackedColor,
    flags: crate::CellFlags,
}

// Keep the scalar decision inside each row-fill specialization rather than
// passing a row and returning a staged extra through a call for every cell.
#[inline(always)]
fn place_line_cell<'a>(
    row: &mut Row,
    col: u16,
    cols: u16,
    unit_str: &'a str,
    chars_consumed: usize,
    style: &LineCellStyle,
    is_wide: bool,
) -> (u16, Option<RestoredExtra<'a>>) {
    let c = super::scroll_materialize::char_at(unit_str, 0);
    let base_is_bmp = (c as u32) <= Cell::MAX_DIRECT_CODEPOINT;

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
            Some(RestoredExtra::CombiningMarks(
                col,
                &unit_str[c.len_utf8()..],
            ))
        } else {
            None
        };
        (new_col, extra)
    } else {
        let new_col = set_complex_cell(row, col, cols, style, is_wide);
        let extra = (new_col > col).then_some({
            if chars_consumed == 1 {
                RestoredExtra::NonBmpChar(col, c)
            } else {
                RestoredExtra::ComplexChar(col, unit_str)
            }
        });
        (new_col, extra)
    }
}

fn set_cell(row: &mut Row, col: u16, cols: u16, c: char, s: &LineCellStyle, is_wide: bool) -> u16 {
    use crate::CellFlags;
    let flags = if is_wide {
        s.flags.union(CellFlags::WIDE)
    } else {
        s.flags.difference(CellFlags::WIDE)
    };
    if is_wide && col + 1 < cols {
        row.set(col, Cell::with_style(c, s.fg, s.bg, flags));
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

fn set_complex_cell(row: &mut Row, col: u16, cols: u16, s: &LineCellStyle, is_wide: bool) -> u16 {
    use crate::CellFlags;
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
        col.saturating_add(2)
    } else if is_wide {
        col
    } else {
        row.set(col, cell);
        col.saturating_add(1)
    }
}

fn apply_row_extra(
    extras: &mut CellExtras,
    row_idx: u16,
    visible_rows: u16,
    cols: u16,
    extra: RestoredExtra<'_>,
    owned_text: &mut bool,
) {
    match extra {
        RestoredExtra::NonBmpChar(col, ch) => {
            extras.set_complex_char_ring(row_idx, col, ch, visible_rows, cols);
        }
        RestoredExtra::CombiningMarks(col, marks) => {
            extras.modify(CellCoord::new(row_idx, col), |entry| {
                for mark in marks.chars() {
                    entry.add_combining(mark);
                }
                *owned_text |= entry.combining().len() > 2;
            });
        }
        RestoredExtra::ComplexChar(col, s) => {
            extras.modify(CellCoord::new(row_idx, col), |entry| {
                entry.refill_complex_char(s)
            });
            *owned_text = true;
        }
        RestoredExtra::RgbFg(col, rgb) => {
            extras.modify(CellCoord::new(row_idx, col), |entry| {
                entry.set_fg_rgb(Some(rgb))
            });
        }
        RestoredExtra::RgbBg(col, rgb) => {
            extras.modify(CellCoord::new(row_idx, col), |entry| {
                entry.set_bg_rgb(Some(rgb))
            });
        }
    }
}

#[cfg(test)]
#[path = "scroll_fill_tests.rs"]
mod tests;
