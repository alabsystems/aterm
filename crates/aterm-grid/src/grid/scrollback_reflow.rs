// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Off-screen scrollback reflow on width changes.

use aterm_rle::Rle;
use aterm_scrollback::{CellAttrs, HyperlinkSpan, ImageSpan, Line, UnderlineColorSpan};
use std::{collections::BTreeMap, ops::Range, sync::Arc};

use super::Grid;

impl Grid {
    // COST: UNBOUNDED(ring+tiered-history-lines) — materializes the entire
    // off-screen scrollback. Must stay behind the `resize_offloading_scrollback`
    // detach on any main-thread-reachable path (`aterm-census --mainloop`).
    /// Extract the whole off-screen scrollback as logical [`Line`]s.
    pub(super) fn take_scrollback_lines(&mut self) -> Vec<Line> {
        self.drain_lazy_buffer();
        let mut lines = Vec::new();
        if let Some(scrollback) = self.storage.scrollback.as_mut() {
            let count = scrollback.line_count();
            lines.reserve(count);
            for i in 0..count {
                match scrollback.get_line(i) {
                    Ok(Some(line)) => lines.push(line.into_owned()),
                    Ok(None) | Err(_) => lines.push(Line::new()),
                }
            }
            if let Err(error) = scrollback.clear() {
                aterm_log::warn!("scrollback clear during reflow failed: {error}");
            }
        }

        lines.extend(self.take_ring_scrollback_lines());

        lines
    }

    // COST: O(ring-scrollback-rows): bounded by the ring capacity (a
    // construction-time constant), not by session history.
    /// Extract only the ring's scrollback rows as logical [`Line`]s (oldest first).
    pub(super) fn take_ring_scrollback_lines(&mut self) -> Vec<Line> {
        self.take_older_ring_scrollback_lines(0)
    }

    // COST: O(ring-scrollback-rows): bounded by the ring capacity (a
    // construction-time constant), not by session history.
    /// Extract the ring's scrollback rows but the `keep` newest as logical [`Line`]s.
    pub(super) fn take_older_ring_scrollback_lines(&mut self, keep: usize) -> Vec<Line> {
        let ring_scrollback = self.storage.ring_buffer_scrollback();
        let take = ring_scrollback.saturating_sub(keep);
        if take == 0 {
            return Vec::new();
        }
        self.storage.linearize_ring();
        let mut lines = Vec::with_capacity(take);
        let no_extras = super::ScrolledRowExtras::default();
        for i in 0..take {
            let extras = self.storage.ring_history_extras(i).unwrap_or(&no_extras);
            let row = &self.storage.rows[i];
            let len = history_row_len(row, self.storage.rows.get(i + 1));
            lines.push(Self::row_to_line_with_stored_extras_at_len(
                row, extras, len,
            ));
        }
        self.storage.recycle_rows(0..take);
        if take == ring_scrollback {
            self.storage.ring_extras.clear();
        } else {
            for _ in 0..take {
                self.storage.ring_extras.pop_front();
            }
        }
        self.storage.total_lines -= take;
        lines
    }

    // COST: O(held rows): the scan stops at the cell budget.
    /// How many of the ring's newest scrollback rows an offloaded width change holds back.
    pub(super) fn ring_rows_to_hold_back(&self, cells: usize) -> usize {
        let ring = self.storage.ring_buffer_scrollback();
        let cols = usize::from(self.storage.cols).max(1);
        let max_rows = (cells / cols).min(ring);
        let mut held = 0;
        for back in 1..=max_rows {
            let Some(row) = self.storage.ring_history_row(ring - back) else {
                break;
            };
            if !row.is_wrapped() {
                held = back;
            }
        }
        held
    }

    /// Push rewrapped scrollback [`Line`]s back into history as its oldest part.
    pub(super) fn restore_reflowed_scrollback(
        &mut self,
        mut lines: Vec<Line>,
        new_cols: u16,
        staged_before: usize,
    ) {
        if self.storage.store_retains_lines() {
            if lines.is_empty() {
                return;
            }
            if let Some(scrollback) = self.storage.scrollback.as_mut() {
                for line in lines {
                    if let Err(error) = scrollback.push_line(line) {
                        aterm_log::warn!("scrollback push_line during reflow failed: {error}");
                    }
                }
            }
            if self.storage.lazy_buffer.is_empty() {
                self.enforce_scrollback_budget_and_clamp();
            } else {
                self.drain_lazy_buffer();
            }
        } else {
            let overflow = self.storage.lazy_buffer.len().saturating_sub(staged_before);
            if overflow > 0 {
                lines.extend(self.storage.lazy_buffer.drain_back(overflow));
            }
            if !lines.is_empty() {
                self.prepend_ring_scrollback_lines(lines, new_cols);
            }
        }
    }

    /// Convert reflowed scrollback [`Line`]s into ring scrollback rows and prepend them.
    pub(super) fn prepend_ring_scrollback_lines(&mut self, lines: Vec<Line>, new_cols: u16) {
        let cap = self.storage.max_scrollback;
        let skip = lines.len().saturating_sub(cap);
        if skip > 0 && self.storage.stages_evicted_rows() {
            for line in &lines[..skip] {
                let (row, extras) = self.build_scrollback_row(line, new_cols);
                self.storage.lazy_buffer.push_row(&row, extras);
                // SAFETY: build_scrollback_row allocated this temporary row in
                // self.storage.pages; lazy_buffer owns its copied cell data.
                unsafe { row.recycle(&mut self.storage.pages) };
            }
        }
        let kept = &lines[skip..];
        if kept.is_empty() {
            return;
        }
        let added = self.seat_lines_at_ring_front(kept, new_cols);
        self.renumber_rows(
            self.storage
                .absolute_row_counter
                .saturating_add(added as u64),
        );
        self.storage.history_renumber_epoch = self.storage.history_renumber_epoch.saturating_add(1);
        self.enforce_history_link_budget();
    }

    /// Seat `lines` (oldest first) at the front of the ring.
    pub(super) fn seat_lines_at_ring_front(&mut self, lines: &[Line], cols: u16) -> usize {
        if lines.is_empty() {
            return 0;
        }
        self.storage.linearize_ring();

        let mut new_rows = Vec::with_capacity(lines.len());
        let mut new_extras = Vec::with_capacity(lines.len());
        for line in lines {
            let (row, extras) = self.build_scrollback_row(line, cols);
            new_rows.push(row);
            new_extras.push(if extras.is_empty() {
                None
            } else {
                Some(Box::new(extras))
            });
        }

        let added = new_rows.len();
        let visible: Vec<_> = std::mem::take(&mut self.storage.rows);
        new_rows.extend(visible);
        self.storage.rows = new_rows;
        for (i, extra) in new_extras.into_iter().enumerate() {
            self.storage.ring_extras.insert(i, extra);
        }
        self.storage.total_lines = self.storage.rows.len();
        added
    }

    pub(super) fn build_scrollback_row(
        &mut self,
        line: &Line,
        new_cols: u16,
    ) -> (crate::Row, super::ScrolledRowExtras) {
        // SAFETY: the caller moves the row into `self.storage.rows` and owns `self.storage.pages`
        // for at least as long as the row lives.
        let mut row = unsafe { crate::Row::new(new_cols, &mut self.storage.pages) };
        let extras = super::scroll_fill::fill_row_into(&mut row, line, new_cols);
        (row, extras)
    }
}

/// How many cells of a history row (one leaving the grid as a [`Line`]) are its logical content.
pub(super) fn history_row_len(row: &crate::Row, successor: Option<&crate::Row>) -> u16 {
    if row.line_size() == super::LineSize::SingleWidth
        && successor.is_some_and(crate::Row::is_wrapped)
    {
        row.continued_len()
    } else {
        row.len()
    }
}

const MAX_LOGICAL_WIDTH: usize = crate::MAX_GRID_COLS as usize * crate::MAX_GRID_ROWS as usize;

#[cfg(test)]
thread_local! {
    static BUILT_HISTORY_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

struct Unit<'a> {
    /// The grapheme text for the cell (base char plus combining marks / zwj).
    text: &'a str,
    /// Attributes for the cell.
    attrs: CellAttrs,
    /// Source hyperlink span covering this cell; output rows acquire ownership.
    link: Option<&'a HyperlinkSpan>,
    /// Packed SGR 58 underline colour covering this cell.
    underline_color: Option<u32>,
    /// Display width (1 or 2).
    width: u16,
}

// COST: UNBOUNDED(session-history-cells) — rewraps O(total cells) of history.
/// Rewrap scrollback [`Line`]s to `new_cols`, keeping hard newlines and content.
#[must_use]
pub(super) fn reflow_scrollback_lines(lines: &[Line], new_cols: u16) -> Vec<Line> {
    let new_cols = new_cols.max(1);
    let mut out: Vec<Line> = Vec::with_capacity(lines.len());
    let mut units: Vec<Unit<'_>> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let start = i;
        i += 1;
        while i < lines.len() && lines[i].is_wrapped() {
            i += 1;
        }
        let run = &lines[start..i];
        if run.len() == 1 && rewrap_passthrough_eligible(&run[0], new_cols) {
            #[cfg(any(test, debug_assertions))]
            super::count_reflow_passthrough_lines(1);
            out.push(run[0].cloned_for_rewrap());
            continue;
        }
        emit_logical_line(run, new_cols, &mut units, &mut out, None);
    }
    out
}

/// The rows [`reflow_scrollback_lines`]`(lines, new_cols)` makes.
// COST: O(lines + decoded units), with constant mapping scratch.
#[must_use]
pub(super) fn rewrap_cell_row(
    lines: &[Line],
    new_cols: u16,
    line: usize,
    cells: usize,
) -> (usize, Option<(usize, u16)>) {
    let cols = usize::from(new_cols.max(1));
    let mut point = None;
    let mut rows = 0usize;
    let mut i = 0;
    while i < lines.len() {
        let start = i;
        i += 1;
        while i < lines.len() && lines[i].is_wrapped() {
            i += 1;
        }
        let (mut col, mut row, mut seen) = (0usize, 0usize, 0usize);
        let mut pending = false;
        for (index, source) in lines[start..i].iter().enumerate() {
            let selected = start + index == line;
            if selected {
                pending = true;
            }
            if seen == MAX_LOGICAL_WIDTH {
                continue;
            }
            let Some(text) = source.as_str() else {
                continue;
            };
            let mut consumed = 0usize;
            for stored in super::scroll_materialize::StoredUnits::new(source, text)
                .take(MAX_LOGICAL_WIDTH - seen)
            {
                let width =
                    if super::scroll_materialize::stored_unit_is_wide(stored.unit, stored.attrs) {
                        2
                    } else {
                        1
                    };
                if col + width > cols && col > 0 {
                    row += 1;
                    col = 0;
                }
                // A soft source-row end resolves at the next unit, after its
                // wrap; a hard logical end resolves at the final position below.
                if pending && (!selected || consumed + width > cells) {
                    point = Some((rows + row, u16::try_from(col).unwrap_or(u16::MAX)));
                    pending = false;
                }
                if selected {
                    consumed += width;
                }
                col += width;
                seen += 1;
            }
        }
        if pending {
            point = Some((rows + row, u16::try_from(col).unwrap_or(u16::MAX)));
        }
        rows += row + 1;
    }
    (rows, point)
}

// COST: O(rows × max(1, new_cols) + one logical line): visited logical lines
// are scanned, but only retained rows are materialized.
/// The newest `rows` rows of [`reflow_scrollback_lines`]`(lines, new_cols)`.
#[must_use]
pub(super) fn reflow_newest_rows(lines: &[Line], new_cols: u16, rows: usize) -> Vec<Line> {
    let new_cols = new_cols.max(1);
    let mut runs: Vec<Vec<Line>> = Vec::new();
    let mut units = Vec::new();
    let mut produced = 0;
    let mut end = lines.len();
    while end > 0 && produced < rows {
        let head = lines[..end]
            .iter()
            .rposition(|line| !line.is_wrapped())
            .unwrap_or(0);
        let source = &lines[head..end];
        let mut run = Vec::new();
        if source.len() == 1 && rewrap_passthrough_eligible(&source[0], new_cols) {
            #[cfg(any(test, debug_assertions))]
            super::count_reflow_passthrough_lines(1);
            run.push(source[0].cloned_for_rewrap());
        } else {
            emit_logical_line(
                source,
                new_cols,
                &mut units,
                &mut run,
                Some(rows - produced),
            );
        }
        produced += run.len();
        runs.push(run);
        end = head;
    }
    runs.into_iter().rev().flatten().collect()
}

/// The full-decomposition rewrap with passthrough disabled: the reference for
/// the passthrough differential tests.
#[cfg(test)]
#[must_use]
pub(super) fn reflow_scrollback_lines_reference(lines: &[Line], new_cols: u16) -> Vec<Line> {
    let new_cols = new_cols.max(1);
    let mut out: Vec<Line> = Vec::with_capacity(lines.len());
    let mut units: Vec<Unit<'_>> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let start = i;
        i += 1;
        while i < lines.len() && lines[i].is_wrapped() {
            i += 1;
        }
        emit_logical_line(&lines[start..i], new_cols, &mut units, &mut out, None);
    }
    out
}

fn rewrap_passthrough_eligible(line: &Line, new_cols: u16) -> bool {
    if line.hyperlinks().is_some_and(|spans| !spans.is_empty())
        || line
            .underline_colors()
            .is_some_and(|spans| !spans.is_empty())
        || line.has_images()
    {
        return false;
    }
    let Some(text) = line.as_str() else {
        return false;
    };
    if text.len() > new_cols as usize || !text.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
        return false;
    }
    line.attrs().is_none_or(|rle| {
        rle.runs().iter().all(|run| {
            !crate::CellFlags::from_bits(run.value.flags).contains(crate::CellFlags::WIDE)
        })
    })
}

fn emit_logical_line<'a>(
    run: &'a [Line],
    new_cols: u16,
    units: &mut Vec<Unit<'a>>,
    out: &mut Vec<Line>,
    tail_rows: Option<usize>,
) {
    units.clear();
    if tail_rows == Some(0) {
        return;
    }
    let first_out = out.len();
    let mut image_rows: aterm_alloc::SmallVec<ImageSourceRow<'_>, 4> = aterm_alloc::SmallVec::new();
    for line in run {
        let start = units.len();
        collect_units(line, units);
        if let Some(spans) = line.images() {
            image_rows.push(ImageSourceRow {
                units: start..units.len(),
                spans,
            });
        }
        if units.len() >= MAX_LOGICAL_WIDTH {
            break;
        }
    }

    if units.is_empty() {
        out.push(Line::new());
        carry_image_spans(&image_rows, units, new_cols, out, first_out, 0);
        return;
    }

    let cols = new_cols as usize;
    let skip_rows = tail_rows.map_or(0, |keep| {
        let (mut rows, mut col) = (1usize, 0usize);
        for unit in units.iter() {
            let width = usize::from(unit.width);
            if col + width > cols && col > 0 {
                rows += 1;
                col = 0;
            }
            col += width;
        }
        rows.saturating_sub(keep)
    });
    let mut col = 0usize;
    let mut seg_start = 0usize;
    let mut row = 0usize;
    let mut idx = 0usize;
    while idx < units.len() {
        let w = units[idx].width as usize;
        if col + w > cols && col > 0 {
            if row >= skip_rows {
                out.push(build_line(&units[seg_start..idx], row != 0));
            }
            row += 1;
            seg_start = idx;
            col = 0;
        }
        col += w;
        idx += 1;
    }
    if row >= skip_rows {
        out.push(build_line(&units[seg_start..], row != 0));
    }
    carry_image_spans(&image_rows, units, new_cols, out, first_out, skip_rows);
}

struct ImageSourceRow<'a> {
    units: Range<usize>,
    spans: &'a [ImageSpan],
}

struct ImageRowSegment {
    source: Range<usize>,
    row: usize,
    col: usize,
}

/// Project image-covered text cells through the text's greedy wrap. Tiles past
/// the source text stay relative to that row's origin and clip at the new edge.
fn carry_image_spans(
    sources: &[ImageSourceRow<'_>],
    units: &[Unit<'_>],
    new_cols: u16,
    out: &mut [Line],
    first_out: usize,
    skipped: usize,
) {
    if sources.is_empty() {
        return;
    }
    let cols = usize::from(new_cols);
    let (mut next, mut row, mut col) = (0, 0, 0);
    let mut images = ImageProjection {
        rows: BTreeMap::new(),
        cols: new_cols,
        first_out,
        skipped,
        out_len: out.len(),
    };
    let mut segments: aterm_alloc::SmallVec<ImageRowSegment, 4> = aterm_alloc::SmallVec::new();
    for source in sources {
        for unit in &units[next..source.units.start] {
            let width = usize::from(unit.width);
            if col + width > cols && col > 0 {
                row += 1;
                col = 0;
            }
            col += width;
        }
        // A soft row origin resolves after the next unit's early wrap, just
        // like rewrap_cell_row's cursor projection.
        if let Some(unit) = units.get(source.units.start)
            && col + usize::from(unit.width) > cols
            && col > 0
        {
            row += 1;
            col = 0;
        }
        let origin = (row, col);
        let mut source_col = 0;
        let image_end = source
            .spans
            .iter()
            .map(|span| usize::from(span.end_col))
            .max()
            .unwrap_or(0);
        next = source.units.start;
        segments.clear();
        for unit in &units[source.units.clone()] {
            if source_col >= image_end {
                break;
            }
            let width = usize::from(unit.width);
            if col + width > cols && col > 0 {
                row += 1;
                col = 0;
            }
            if let Some(last) = segments.last_mut()
                && last.row == row
                && last.col + last.source.len() == col
            {
                last.source.end += width;
            } else {
                segments.push(ImageRowSegment {
                    source: source_col..source_col + width,
                    row,
                    col,
                });
            }
            source_col += width;
            col += width;
            next += 1;
        }
        for span in source.spans {
            let start = usize::from(span.start_col);
            let end = usize::from(span.end_col);
            let at = segments.partition_point(|segment| segment.source.end <= start);
            for segment in segments[at..]
                .iter()
                .take_while(|segment| segment.source.start < end)
            {
                let from = start.max(segment.source.start);
                let to = end.min(segment.source.end);
                images.push(
                    span,
                    from..to,
                    segment.row,
                    segment.col + from - segment.source.start,
                );
            }
            if end > source_col {
                let from = start.max(source_col);
                images.push(span, from..end, origin.0, origin.1 + from);
            }
        }
    }
    for (target, mut spans) in images.rows {
        spans.sort_by_key(|span| aterm_types::layer_order(span.image.z_index, span.kitty));
        out[target].set_images(spans);
    }
}

struct ImageProjection {
    rows: BTreeMap<usize, Vec<ImageSpan>>,
    cols: u16,
    first_out: usize,
    skipped: usize,
    out_len: usize,
}

impl ImageProjection {
    fn push(&mut self, span: &ImageSpan, source: Range<usize>, row: usize, col: usize) {
        if source.is_empty() || col >= usize::from(self.cols) || row < self.skipped {
            return;
        }
        let target = self.first_out + row - self.skipped;
        if target >= self.out_len {
            return;
        }
        let Some(first_cell_col) = span.first_cell_col.checked_add(
            u16::try_from(source.start - usize::from(span.start_col)).unwrap_or(u16::MAX),
        ) else {
            return;
        };
        let start_col = u16::try_from(col).unwrap_or(u16::MAX);
        let end_col =
            u16::try_from((col + source.len()).min(usize::from(self.cols))).unwrap_or(self.cols);
        let spans = self.rows.entry(target).or_default();
        if let Some(last) = spans.last_mut()
            && Arc::ptr_eq(&last.image, &span.image)
            && last.kitty == span.kitty
            && last.image_row == span.image_row
            && last.end_col == start_col
            && last
                .first_cell_col
                .checked_add(last.end_col - last.start_col)
                == Some(first_cell_col)
        {
            last.end_col = end_col;
        } else {
            spans.push(ImageSpan {
                start_col,
                end_col,
                first_cell_col,
                ..span.clone()
            });
        }
    }
}

fn collect_units<'a>(line: &'a Line, units: &mut Vec<Unit<'a>>) {
    let Some(text) = line.as_str() else {
        return;
    };
    let mut col: u16 = 0;
    let spans = line.hyperlinks().unwrap_or(&[]);
    debug_assert!(
        spans.windows(2).all(|w| w[0].start_col <= w[1].start_col),
        "hyperlink spans must be sorted by start_col for the cursor scan"
    );
    let mut span_idx = 0usize;
    let ul_spans = line.underline_colors().unwrap_or(&[]);
    debug_assert!(
        ul_spans
            .windows(2)
            .all(|w| w[0].start_col <= w[1].start_col),
        "underline-colour spans must be sorted by start_col for the cursor scan"
    );
    let mut ul_idx = 0usize;
    for stored in super::scroll_materialize::StoredUnits::new(line, text) {
        let attrs = stored.attrs;
        let width = if super::scroll_materialize::stored_unit_is_wide(stored.unit, attrs) {
            2
        } else {
            1
        };
        while span_idx < spans.len() && spans[span_idx].end_col <= col {
            span_idx += 1;
        }
        let link = spans.get(span_idx).filter(|s| s.contains(col));
        while ul_idx < ul_spans.len() && ul_spans[ul_idx].end_col <= col {
            ul_idx += 1;
        }
        let underline_color = ul_spans
            .get(ul_idx)
            .filter(|s| s.contains(col))
            .map(|s| s.color);
        units.push(Unit {
            text: stored.text,
            attrs,
            link,
            underline_color,
            width,
        });
        col = col.saturating_add(width);
        if units.len() >= MAX_LOGICAL_WIDTH {
            break;
        }
    }
}

fn build_line(units: &[Unit<'_>], wrapped: bool) -> Line {
    #[cfg(test)]
    BUILT_HISTORY_ROWS.with(|count| count.set(count.get() + 1));
    let mut text = String::with_capacity(
        units
            .iter()
            .map(|unit| unit.text.len())
            .fold(0usize, usize::saturating_add),
    );
    let mut attrs_rle: Rle<CellAttrs> = Rle::new();
    let mut spans: Vec<HyperlinkSpan> = Vec::new();
    let mut open: Option<(u16, &HyperlinkSpan)> = None;
    let mut ul_spans: Vec<UnderlineColorSpan> = Vec::new();
    let mut ul_open: Option<(u16, u32)> = None;
    let mut col: u16 = 0;

    let mut prev: Option<usize> = None;
    for unit in units {
        let at = text.len();
        text.push_str(unit.text);
        let opens =
            prev.is_some_and(|prev| super::scroll_materialize::walk_folds_across(&text, prev, at));
        prev = Some(at);
        attrs_rle.push(unit.attrs.with_opens_cell(opens));
        if unit.text.len() > 1 {
            for _ in 1..unit.text.chars().count() {
                attrs_rle.push(unit.attrs);
            }
        }
        match (open, unit.link) {
            (None, Some(link)) => open = Some((col, link)),
            (Some((_, previous)), Some(link))
                if crate::HyperlinkData::same_scope(&previous.payload, &link.payload) => {}
            (Some((start, previous)), next) => {
                spans.push(previous.with_range(start, col));
                open = next.map(|link| (col, link));
            }
            (None, None) => {}
        }
        match (ul_open, unit.underline_color) {
            (None, Some(color)) => ul_open = Some((col, color)),
            (Some((_, ocolor)), Some(color)) if ocolor == color => {}
            (Some((start, ocolor)), next) => {
                ul_spans.push(UnderlineColorSpan::new(start, col, ocolor));
                ul_open = next.map(|color| (col, color));
            }
            (None, None) => {}
        }
        col = col.saturating_add(unit.width);
    }
    if let Some((start, link)) = open {
        spans.push(link.with_range(start, col));
    }
    if let Some((start, color)) = ul_open {
        ul_spans.push(UnderlineColorSpan::new(start, col, color));
    }

    let mut line = Line::with_hyperlinks_owned(text, attrs_rle, spans);
    if !ul_spans.is_empty() {
        line.set_underline_colors(ul_spans);
    }
    line.set_wrapped(wrapped);
    line
}

#[cfg(test)]
#[path = "scrollback_reflow_tests.rs"]
mod tests;
