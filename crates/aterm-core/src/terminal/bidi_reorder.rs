// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! BiDi visual-reordering bridge between [`Terminal`] rows and [`aterm_bidi`].

use super::Terminal;
use aterm_grid::VisibleRowView;
use aterm_types::{BiDiMode, ParagraphDirection};

impl Terminal {
    /// Compute the visual-to-logical cell permutation for a rendered row.
    #[must_use]
    pub fn bidi_visual_order_cells(&self, cells: &[super::RenderCell]) -> Vec<usize> {
        // Deliberate terminal fast path: with BiDi off or no codepoint at/past the first RTL
        // block (U+0590), keep pure-Latin/blank rows left-anchored in logical order (even under
        // SCP Rtl/AutoRtl) and skip per-cell scalar/flag allocations.
        if self.modes.bidi_mode == BiDiMode::Disabled || !cells.iter().any(|c| c.ch >= '\u{0590}') {
            return (0..cells.len()).collect();
        }
        let chars: Vec<char> = cells.iter().map(|c| c.ch).collect();
        let wide: Vec<bool> = cells.iter().map(|c| c.wide).collect();
        compute_visual_order_cells(
            self.modes.bidi_mode,
            self.modes.bidi_direction,
            self.modes.bidi_autodetection,
            &chars,
            &wide,
        )
    }

    /// Return visible row `row` in display order (mirrored text and visual-to-logical map).
    #[must_use]
    pub fn row_display_order(&self, row: u16) -> Option<(String, Vec<usize>)> {
        if row >= self.grid.rows() {
            return None;
        }
        let cols = self.grid.cols();
        let view = self.grid.screen_row_view(row);
        Some(
            if self.modes.bidi_mode != BiDiMode::Disabled && may_reorder(&view) {
                self.row_display_order_full(&view, cols)
            } else {
                row_logical_order(&view, cols)
            },
        )
    }

    fn row_display_order_full(&self, view: &VisibleRowView<'_>, cols: u16) -> (String, Vec<usize>) {
        let (chars, continuation) = render_scalars(view);
        let mut levels = aterm_bidi::Scratch::default();
        let mut map = visual_order_cells(
            self.modes.bidi_mode,
            self.modes.bidi_direction,
            self.modes.bidi_autodetection,
            &chars,
            &continuation,
            Some(&mut levels),
        );
        map.extend(map.len()..usize::from(cols));
        let mut text = String::with_capacity(usize::from(cols));
        for &l in &map {
            if let Ok(col) = u16::try_from(l) {
                let start = text.len();
                push_text(view, col, &mut text);
                if levels.cell_is_rtl(l)
                    && let Some(first) = text.get(start..).and_then(|t| t.chars().next())
                    && let Some(mirror) = aterm_bidi::mirror_glyph(first)
                {
                    let end = start + first.len_utf8();
                    text.replace_range(start..end, mirror.encode_utf8(&mut [0; 4]));
                }
            }
        }
        let end = text.trim_end().len();
        text.truncate(end);
        (text, map)
    }

    /// Map a viewport cell `(row, col)` to its logical column and whether it resolved RTL.
    #[must_use]
    pub fn viewport_cell_logical(&self, row: u16, col: u16) -> (u16, bool) {
        if self.modes.bidi_mode == BiDiMode::Disabled {
            return (col, false);
        }
        let view = self.grid().visible_row_view(row);
        if col >= view.len() || !may_reorder(&view) {
            return (col, false);
        }
        let (chars, wide) = render_scalars(&view);
        let visual = usize::from(col);
        if visual >= chars.len() || !chars.iter().any(|&ch| ch >= '\u{0590}') {
            return (col, false);
        }
        let classes: Vec<aterm_bidi::BidiClass> =
            chars.iter().map(|&ch| aterm_bidi::bidi_class(ch)).collect();
        if !aterm_bidi::has_bidi_classes(&classes) {
            return (col, false);
        }
        let brackets: Vec<aterm_bidi::Bracket> =
            chars.iter().map(|&ch| aterm_bidi::bracket_of(ch)).collect();
        let base = base_direction_from_classes(
            self.modes.bidi_direction,
            self.modes.bidi_autodetection,
            &classes,
        );
        // The permutation and direction must come from the same resolution. Expanding
        // cell levels separately would resolve the entire row a second time per hit.
        let mut levels = aterm_bidi::Scratch::default();
        let mut order = Vec::new();
        aterm_bidi::reorder_cells_with_classes_into(
            &classes,
            &brackets,
            &wide,
            base,
            &mut levels,
            &mut order,
        );
        let logical = order.get(visual).copied().unwrap_or(visual);
        let rtl = levels.cell_is_rtl(logical);
        (u16::try_from(logical).unwrap_or(col), rtl)
    }

    /// Map a painted viewport cell `(row, col)` to its logical column and RTL flag.
    #[inline]
    #[must_use]
    pub fn painted_cell_logical(&self, row: u16, col: u16) -> (u16, bool) {
        use crate::grid::LineSize;
        let col = match self.grid().row(row).map(crate::grid::Row::line_size) {
            Some(
                LineSize::DoubleWidth | LineSize::DoubleHeightTop | LineSize::DoubleHeightBottom,
            ) => col / 2,
            _ => col,
        };
        self.viewport_cell_logical(row, col)
    }

    /// Reorder each row of a render snapshot into BiDi visual order in place.
    pub(crate) fn apply_bidi_reorder(
        &mut self,
        frame: &mut crate::render::RenderInput,
        refill_mask: Option<&[u64]>,
    ) -> bool {
        if self.modes.bidi_mode == BiDiMode::Disabled {
            return false;
        }
        let mut reordered_any = false;
        let dir = self.modes.bidi_direction;
        let autodetect = self.modes.bidi_autodetection;
        let scratch = &mut self.bidi_state.scratch;
        for r in 0..frame.cells.len() {
            // A retained (unmasked) row is still the identity (see the doc).
            if let Some(mask) = refill_mask
                && mask.get(r / 64).is_none_or(|w| w & (1u64 << (r % 64)) == 0)
            {
                continue;
            }
            {
                let row = &frame.cells[r];
                if !row.iter().any(|c| c.ch >= '\u{0590}') {
                    continue;
                }
                scratch.classes.clear();
                scratch.wide.clear();
                scratch
                    .classes
                    .extend(row.iter().map(|c| aterm_bidi::bidi_class(c.ch)));
                scratch.wide.extend(row.iter().map(|c| c.wide));
            }
            if !aterm_bidi::has_bidi_classes(&scratch.classes) {
                continue;
            }
            let base = base_direction_from_classes(dir, autodetect, &scratch.classes);
            scratch.brackets.clear();
            scratch
                .brackets
                .extend(frame.cells[r].iter().map(|c| aterm_bidi::bracket_of(c.ch)));
            aterm_bidi::reorder_cells_with_classes_into(
                &scratch.classes,
                &scratch.brackets,
                &scratch.wide,
                base,
                &mut scratch.bidi,
                &mut scratch.cell_order,
            );
            if scratch.cell_order.iter().enumerate().all(|(v, &l)| v == l) {
                continue;
            }
            scratch.inv.clear();
            scratch.inv.resize(scratch.cell_order.len(), 0usize);
            for (v, &l) in scratch.cell_order.iter().enumerate() {
                scratch.inv[l] = v;
            }
            scratch.row_tmp.clear();
            let levels = &scratch.bidi;
            scratch.row_tmp.extend(scratch.cell_order.iter().map(|&l| {
                let mut cell = frame.cells[r][l];
                if levels.cell_is_rtl(l)
                    && let Some(mirror) = aterm_bidi::mirror_glyph(cell.ch)
                {
                    cell.ch = mirror;
                }
                cell
            }));
            std::mem::swap(&mut frame.cells[r], &mut scratch.row_tmp);
            fn remap_cols<T>(entries: &mut [(usize, T)], inv: &[usize]) {
                for (c, _) in entries.iter_mut() {
                    if *c < inv.len() {
                        *c = inv[*c];
                    }
                }
                entries.sort_by_key(|(c, _)| *c);
            }
            if let Some(entries) = frame.clusters.get_mut(r) {
                remap_cols(entries, &scratch.inv);
            }
            if let Some(entries) = frame.combining.get_mut(r) {
                remap_cols(entries, &scratch.inv);
            }
            if let Some(entries) = frame.images.get_mut(r) {
                remap_cols(entries, &scratch.inv);
            }
            if frame.cursor_row == r && frame.cursor_col < scratch.inv.len() {
                frame.cursor_col = scratch.inv[frame.cursor_col];
            }
            if let Some(map) = frame.visual_to_logical.get_mut(r) {
                map.clear();
                map.extend(
                    scratch
                        .cell_order
                        .iter()
                        .map(|&l| u16::try_from(l).unwrap_or(u16::MAX)),
                );
            }
            reordered_any = true;
        }
        reordered_any
    }
}

/// Whether `view` holds a stored codepoint at or past the first RTL block
/// (U+0590): the guard of [`Terminal::bidi_visual_order_cells`], read off the
/// cells before anything is gathered.
#[inline]
fn may_reorder(view: &VisibleRowView<'_>) -> bool {
    (0..view.len()).any(|c| view.cell(c).is_some_and(|cell| cell.char() >= '\u{0590}'))
}

fn render_scalars(view: &VisibleRowView<'_>) -> (Vec<char>, Vec<bool>) {
    let len = usize::from(view.len());
    let mut chars = Vec::with_capacity(len);
    let mut continuation = Vec::with_capacity(len);
    let mut prev_wide = false;
    for c in 0..view.len() {
        let Some(cell) = view.cell(c) else {
            prev_wide = false;
            continue;
        };
        let is_cont = cell.is_wide_continuation() && !cell.is_wide() && prev_wide;
        prev_wide = cell.is_wide();
        let raw = if cell.is_complex() {
            view.cell_data(c, cell).complex_base().unwrap_or('\u{FFFD}')
        } else {
            cell.char()
        };
        chars.push(if is_cont || raw == '\0' { ' ' } else { raw });
        continuation.push(is_cont);
    }
    (chars, continuation)
}

fn row_logical_order(view: &VisibleRowView<'_>, cols: u16) -> (String, Vec<usize>) {
    let mut text = String::with_capacity(usize::from(cols));
    for col in 0..cols {
        push_text(view, col, &mut text);
    }
    let end = text.trim_end().len();
    text.truncate(end);
    (text, (0..usize::from(cols)).collect())
}

/// Column `col`'s text in a row read: nothing for a wide glyph's continuation
/// or a column past the row.
#[inline]
fn push_text(view: &VisibleRowView<'_>, col: u16, out: &mut String) {
    if !view.is_wide_continuation(col)
        && let Some(cell) = view.cell(col)
    {
        view.push_cell_text(col, cell, out);
    }
}

/// Compute the visual-to-logical cell permutation from BiDi config, scalars, and wide flags.
#[inline]
#[must_use]
pub fn compute_visual_order_cells(
    mode: BiDiMode,
    dir: ParagraphDirection,
    autodetect: bool,
    cell_chars: &[char],
    is_wide_continuation: &[bool],
) -> Vec<usize> {
    visual_order_cells(
        mode,
        dir,
        autodetect,
        cell_chars,
        is_wide_continuation,
        None,
    )
}

fn visual_order_cells(
    mode: BiDiMode,
    dir: ParagraphDirection,
    autodetect: bool,
    cell_chars: &[char],
    is_wide_continuation: &[bool],
    levels: Option<&mut aterm_bidi::Scratch>,
) -> Vec<usize> {
    if mode == BiDiMode::Disabled || !cell_chars.iter().any(|&c| c >= '\u{0590}') {
        return (0..cell_chars.len()).collect();
    }
    let classes: Vec<aterm_bidi::BidiClass> = cell_chars
        .iter()
        .map(|&c| aterm_bidi::bidi_class(c))
        .collect();
    if !aterm_bidi::has_bidi_classes(&classes) {
        return (0..cell_chars.len()).collect();
    }
    let base = base_direction_from_classes(dir, autodetect, &classes);
    let brackets: Vec<aterm_bidi::Bracket> = cell_chars
        .iter()
        .map(|&c| aterm_bidi::bracket_of(c))
        .collect();
    match levels {
        Some(levels) => {
            let mut order = Vec::new();
            aterm_bidi::reorder_cells_with_classes_into(
                &classes,
                &brackets,
                is_wide_continuation,
                base,
                levels,
                &mut order,
            );
            order
        }
        None => {
            aterm_bidi::reorder_cells_with_classes(&classes, &brackets, is_wide_continuation, base)
        }
    }
}

/// Map the engine's [`ParagraphDirection`] onto an `aterm-bidi`
/// `BaseDirection`, reading the row's precomputed per-cell
/// [`BidiClass`](aterm_bidi::BidiClass)es.
#[inline]
fn base_direction_from_classes(
    dir: ParagraphDirection,
    autodetect: bool,
    classes: &[aterm_bidi::BidiClass],
) -> aterm_bidi::BaseDirection {
    use aterm_bidi::{BaseDirection, BidiClass};
    let default_ltr = if autodetect {
        BaseDirection::Auto
    } else {
        BaseDirection::Ltr
    };
    match dir {
        ParagraphDirection::Auto => default_ltr,
        ParagraphDirection::Ltr => BaseDirection::Ltr,
        ParagraphDirection::Rtl => BaseDirection::Rtl,
        ParagraphDirection::AutoRtl => {
            if !autodetect {
                return BaseDirection::Rtl;
            }
            let has_strong = classes
                .iter()
                .any(|&c| matches!(c, BidiClass::L | BidiClass::R | BidiClass::AL));
            if has_strong {
                BaseDirection::Auto
            } else {
                BaseDirection::Rtl
            }
        }
        _ => default_ltr,
    }
}

#[cfg(test)]
#[path = "bidi_reorder_tests.rs"]
mod tests;
