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
            extras
                .combining
                .push((col, aterm_alloc::SmallVec::from_slice(&marks)));
        }
        RestoredExtra::ComplexChar(col, s) => extras.complex_chars.push((col, s)),
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
                .push(aterm_scrollback::HyperlinkSpan::with_id(
                    held.start,
                    held.end,
                    span.url.clone(),
                    span.id.clone(),
                ));
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

    /// Fill a row whose old extras have already been removed by its caller.
    pub(super) fn fill_row_from_line_cleared(&mut self, row_idx: u16, line: &Line, cols: u16) {
        let Some(text) = line.as_str() else {
            if let Some(row) = self.row_mut(row_idx) {
                row.clear();
                row.set_wrapped(line.is_wrapped());
            }
            return;
        };

        let filled = {
            if row_idx >= self.storage.visible_rows {
                return;
            }
            let Some(idx) = self.storage.row_index(row_idx) else {
                return;
            };
            let Some(row) = self.storage.rows.get_mut(idx) else {
                return;
            };
            let extras = &mut self.storage.cursor_state.extras;
            row.clear();
            let filled = fill_row_cells(row, text, line, cols, &mut |row, extra| {
                if let Some(cell) = row.get_mut(extra.col()) {
                    cell.set_has_extras(true);
                }
                apply_row_extra(extras, row_idx, extra);
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
enum RestoredExtra {
    /// Combining marks on a BMP base, attached via `add_combining`.
    CombiningMarks(u16, aterm_alloc::SmallVec<char, 4>),
    /// Non-BMP / ZWJ sequence: store as complex char string.
    ComplexChar(u16, Arc<str>),
    /// RGB foreground color from scrollback Line attrs.
    RgbFg(u16, [u8; 3]),
    /// RGB background color from scrollback Line attrs.
    RgbBg(u16, [u8; 3]),
}

impl RestoredExtra {
    const fn col(&self) -> u16 {
        match self {
            Self::CombiningMarks(col, _)
            | Self::ComplexChar(col, _)
            | Self::RgbFg(col, _)
            | Self::RgbBg(col, _) => *col,
        }
    }
}

/// Lay out cells, emitting each extra after its cell is written so the consumer
/// can mark that cell and store the payload without retaining a batch.
fn fill_row_cells(
    row: &mut Row,
    text: &str,
    line: &Line,
    cols: u16,
    sink: &mut impl FnMut(&mut Row, RestoredExtra),
) -> u16 {
    if let Some(filled) = fill_ascii_row_cells(row, text, line, cols, sink) {
        return filled;
    }
    fill_row_cells_general(row, text, line, cols, sink)
}

/// Printable ASCII with ordinary cell widths needs no grapheme walk. Restrict
/// the whole line to the row's width, so the test is bounded and a following
/// combining character cannot change the last cell we restore.
fn fill_ascii_row_cells(
    row: &mut Row,
    text: &str,
    line: &Line,
    cols: u16,
    sink: &mut impl FnMut(&mut Row, RestoredExtra),
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

fn fill_row_cells_general(
    row: &mut Row,
    text: &str,
    line: &Line,
    cols: u16,
    sink: &mut impl FnMut(&mut Row, RestoredExtra),
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

fn place_line_cell(
    row: &mut Row,
    col: u16,
    cols: u16,
    unit_str: &str,
    chars_consumed: usize,
    style: &LineCellStyle,
    is_wide: bool,
) -> (u16, Option<RestoredExtra>) {
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
            let marks: aterm_alloc::SmallVec<char, 4> = unit_str[c.len_utf8()..].chars().collect();
            Some(RestoredExtra::CombiningMarks(col, marks))
        } else {
            None
        };
        (new_col, extra)
    } else {
        set_complex_cell(row, col, cols, unit_str, style, is_wide)
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

fn set_complex_cell(
    row: &mut Row,
    col: u16,
    cols: u16,
    unit_str: &str,
    s: &LineCellStyle,
    is_wide: bool,
) -> (u16, Option<RestoredExtra>) {
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
        (
            col.saturating_add(2),
            Some(RestoredExtra::ComplexChar(col, Arc::from(unit_str))),
        )
    } else if is_wide {
        (col, None)
    } else {
        row.set(col, cell);
        (
            col.saturating_add(1),
            Some(RestoredExtra::ComplexChar(col, Arc::from(unit_str))),
        )
    }
}

fn apply_row_extra(extras: &mut CellExtras, row_idx: u16, extra: RestoredExtra) {
    match extra {
        RestoredExtra::CombiningMarks(col, marks) => {
            extras.modify(CellCoord::new(row_idx, col), |entry| {
                for mark in marks {
                    entry.add_combining(mark);
                }
            });
        }
        RestoredExtra::ComplexChar(col, s) => {
            extras.modify(CellCoord::new(row_idx, col), |entry| {
                entry.set_complex_char(Some(s))
            });
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
mod tests {
    use super::*;
    use aterm_scrollback::{CellAttrs, Line, Rle};

    fn compare_fillers(line: &Line, cols: u16, expect_fast: bool) {
        let mut fast = Grid::new(1, cols.max(1));
        let mut reference = Grid::new(1, cols.max(1));
        let text = line.as_str().expect("text fixture");
        let mut actual = Vec::new();
        let direct = fill_ascii_row_cells(
            fast.row_mut(0).unwrap(),
            text,
            line,
            cols,
            &mut |_, extra| actual.push(extra),
        );
        assert_eq!(direct.is_some(), expect_fast, "{text:?}, cols={cols}");
        if direct.is_none() {
            assert!(
                actual.is_empty(),
                "a rejected fast path must emit no extras"
            );
        }
        let filled = direct.unwrap_or_else(|| {
            fill_row_cells(
                fast.row_mut(0).unwrap(),
                text,
                line,
                cols,
                &mut |_, extra| actual.push(extra),
            )
        });
        let mut expected = Vec::new();
        let expected_filled = fill_row_cells_general(
            reference.row_mut(0).unwrap(),
            text,
            line,
            cols,
            &mut |_, extra| expected.push(extra),
        );
        assert_eq!(filled, expected_filled);
        assert_eq!(
            actual, expected,
            "restored extras for {text:?}, cols={cols}"
        );
        let (actual, expected) = (fast.row(0).unwrap(), reference.row(0).unwrap());
        assert_eq!(actual.as_slice(), expected.as_slice());
        assert_eq!(actual.len(), expected.len());
        assert_eq!(actual.flags(), expected.flags());
    }

    #[test]
    fn replacing_a_styled_row_with_plain_text_releases_excess_storage() {
        let cols = 80_u16;
        let attrs = CellAttrs::new(
            crate::PackedColor::rgb(12, 34, 56).0,
            crate::PackedColor::rgb(65, 43, 21).0,
            crate::CellFlags::BOLD.bits(),
        );
        let styled = Line::with_attrs(
            &"x".repeat(usize::from(cols)),
            Rle::with_value(attrs, u32::from(cols)),
        );
        let mut grid = Grid::new(1, cols);
        grid.restore_visible_from_lines(&[styled]);
        let full_capacity = grid.storage.extras.capacity();
        assert!(full_capacity >= usize::from(cols));

        grid.restore_visible_from_lines(&[Line::from("plain")]);
        assert_eq!(grid.storage.extras.len(), 0);
        assert!(grid.storage.extras.capacity() < full_capacity);
        let restored = grid.checkpoint_visible_lines();
        assert_eq!(restored[0].as_str(), Some("plain"));
        let mut attrs = restored[0].attr_cursor();
        for col in 0..5 {
            assert_eq!(attrs.attr_at(col), CellAttrs::DEFAULT);
        }
    }

    #[test]
    fn streamed_visible_extras_match_detached_restoration() {
        let attrs = CellAttrs::new(
            crate::PackedColor::rgb(12, 34, 56).0,
            crate::PackedColor::rgb(65, 43, 21).0,
            crate::CellFlags::BOLD.bits(),
        );
        for text in [
            "ASCII",
            "e\u{301}日👩‍💻Z",
            "日e\u{301}\u{302}\u{303}\u{304}\u{305}X",
        ] {
            let mut line = Line::with_attrs(text, Rle::with_value(attrs, 32));
            line.set_wrapped(true);
            for cols in [1, 2, 3, 7, 16] {
                let mut detached = Grid::new(1, cols);
                let row = detached.row_mut(0).expect("fresh row");
                let extras = fill_row_into(row, &line, cols);
                let expected = Grid::row_to_line_with_stored_extras(row, &extras);
                let mut visible = Grid::new(1, cols);
                visible.restore_visible_from_lines(std::slice::from_ref(&line));
                let actual = visible.checkpoint_visible_lines();
                assert_eq!(actual[0].as_str(), expected.as_str(), "{text:?}, {cols}");
                assert_eq!(actual[0].is_wrapped(), expected.is_wrapped());
                let mut actual_attrs = actual[0].attr_cursor();
                let mut expected_attrs = expected.attr_cursor();
                for col in 0..usize::from(cols) {
                    assert_eq!(
                        actual_attrs.attr_at(col),
                        expected_attrs.attr_at(col),
                        "{text:?}, width {cols}, column {col}"
                    );
                }
            }
        }
    }

    #[test]
    fn ascii_restore_matches_general_cells_styles_lengths_and_holes() {
        let palette = [
            CellAttrs::DEFAULT,
            CellAttrs::new(0x01_112233, 0x01_aabbcc, CellAttrs::OPENS_CELL),
            CellAttrs::new(
                crate::PackedColor::indexed(10).0,
                crate::PackedColor::indexed(3).0,
                crate::CellFlags::BOLD.bits(),
            ),
            CellAttrs::new(0x03_abcdef, 0x04_123456, crate::CellFlags::PROTECTED.bits()),
            CellAttrs::new(0xFF_123456, 0xFF_654321, crate::CellFlags::COMPLEX.bits()),
        ];
        for cols in [0_u16, 1, 2, 7, 80, 256] {
            for len in [0, cols.saturating_sub(1), cols] {
                let text: String = "A~ 09".chars().cycle().take(usize::from(len)).collect();
                compare_fillers(&Line::from(text.as_str()), cols, true);
                let mut attrs = Rle::new();
                for i in 0..usize::from(len) {
                    attrs.push(palette[(i / 3) % palette.len()]);
                }
                compare_fillers(&Line::with_attrs(&text, attrs), cols, true);
                let mut short_attrs = Rle::new();
                for _ in 0..len / 2 {
                    short_attrs.push(palette[1]);
                }
                compare_fillers(&Line::with_attrs(&text, short_attrs), cols, true);
            }
        }
    }

    #[test]
    fn ascii_restore_does_not_scan_attributes_past_the_text() {
        let mut attrs = Rle::with_value(CellAttrs::DEFAULT, 1);
        for i in 0..4096 {
            attrs.push(CellAttrs::new(i % 2, 0, 0));
        }
        attrs.push(CellAttrs::new(0, 0, crate::CellFlags::WIDE.bits()));
        compare_fillers(&Line::with_attrs("a", attrs.clone()), 3, true);
        compare_fillers(&Line::with_attrs("", attrs), 3, true);
    }

    #[test]
    fn ascii_restore_keeps_unicode_wide_controls_and_truncation_on_general_walk() {
        for text in [
            "a\u{0301}",
            "界",
            "a\u{200d}👩",
            "\t",
            "\x7f",
            "a\nb",
            "longer-than-row",
        ] {
            compare_fillers(&Line::from(text), 5, false);
        }
        let mut wide = Rle::new();
        wide.push(CellAttrs::new(
            0xFF_FFFFFF,
            0xFF_000000,
            crate::CellFlags::WIDE.bits(),
        ));
        compare_fillers(&Line::with_attrs("a", wide), 5, false);
    }

    #[test]
    fn fill_row_from_line_replaces_exactly_its_row() {
        let text = |grid: &Grid, row: u16| -> String {
            (0..grid.cols())
                .map(|c| grid.cell(row, c).map_or('?', |cell| cell.char()))
                .collect()
        };
        let cases: &[(&str, &[u8], bool, &str)] = &[
            ("empty", b"", false, "     "),
            ("ascii", b"Hello", true, "Hello"),
            ("short", b"AB", false, "AB   "),
            ("truncated", b"LongTextThatExceedsColumns", false, "LongT"),
        ];
        for &(name, bytes, wrapped, want) in cases {
            let mut grid = Grid::new(3, 5);
            for row in 0..3 {
                grid.set_cursor(row, 0);
                grid.write_char('X');
            }
            let mut line = Line::from_bytes(bytes);
            line.set_wrapped(wrapped);
            grid.fill_row_from_line(1, &line, 5);
            assert_eq!(text(&grid, 1), want, "{name}");
            assert_eq!(grid.row(1).expect("row").is_wrapped(), wrapped, "{name}");
            assert_eq!(text(&grid, 0), "X    ", "{name}: row above");
            assert_eq!(text(&grid, 2), "X    ", "{name}: row below");
            grid.fill_row_from_line(99, &line, 5);
            assert_eq!(text(&grid, 2), "X    ", "{name}: out-of-range target");
        }
        let mut grid = Grid::new(1, 5);
        let line = Line::with_attrs("Hello", Rle::with_value(CellAttrs::DEFAULT, 5));
        grid.fill_row_from_line(0, &line, 5);
        assert_eq!(text(&grid, 0), "Hello", "default attrs");
    }

    #[test]
    fn fill_row_hyperlinks_disjoint_are_correct_and_overlap_is_bounded() {
        use aterm_scrollback::HyperlinkSpan;
        let cols = 4u16;

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

    #[test]
    fn a_glyph_the_row_cannot_hold_takes_its_link_and_underline_colour() {
        use aterm_scrollback::{HyperlinkSpan, UnderlineColorSpan};
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
            let extras = fill_row_into(&mut row, &line, cols);
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
}
