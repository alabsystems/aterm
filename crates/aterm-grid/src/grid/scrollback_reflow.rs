// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Off-screen scrollback reflow on width changes.

use aterm_rle::Rle;
use aterm_scrollback::{CellAttrs, HyperlinkSpan, ImageSpan, Line, UnderlineColorSpan};

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

    fn build_scrollback_row(
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

struct Unit<'a> {
    /// The grapheme text for the cell (base char plus combining marks / zwj).
    text: &'a str,
    /// Attributes for the cell.
    attrs: CellAttrs,
    /// Hyperlink (url, id) covering this cell.
    link: Option<(std::sync::Arc<str>, Option<std::sync::Arc<str>>)>,
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
        emit_logical_line(run, new_cols, &mut units, &mut out);
    }
    out
}

/// The rows [`reflow_scrollback_lines`]`(lines, new_cols)` makes.
// COST: O(cells of `lines`), as the rewrap.
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
    let mut units: Vec<Unit<'_>> = Vec::new();
    let mut firsts: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let start = i;
        i += 1;
        while i < lines.len() && lines[i].is_wrapped() {
            i += 1;
        }
        units.clear();
        firsts.clear();
        for line in &lines[start..i] {
            firsts.push(units.len());
            if units.len() < MAX_LOGICAL_WIDTH {
                collect_units(line, &mut units);
            }
        }
        let mut placed = Vec::with_capacity(units.len());
        let (mut col, mut row) = (0usize, 0usize);
        for unit in &units {
            let w = usize::from(unit.width);
            if col + w > cols && col > 0 {
                row += 1;
                col = 0;
            }
            placed.push((row, col));
            col += w;
        }
        if (start..i).contains(&line) {
            let first = firsts[line - start];
            let end = firsts.get(line - start + 1).copied().unwrap_or(units.len());
            let mut at = first;
            let mut width = 0usize;
            while at < end && width + usize::from(units[at].width) <= cells {
                width += usize::from(units[at].width);
                at += 1;
            }
            let (row, col) = placed.get(at).copied().unwrap_or((row, col));
            point = Some((rows + row, u16::try_from(col).unwrap_or(u16::MAX)));
        }
        rows += row + 1;
    }
    (rows, point)
}

// COST: O(rows × new_cols + one logical line): every logical line rewrapped
// but the oldest one visited yields only kept rows, at most `new_cols` cells
// each.
/// The newest `rows` rows of [`reflow_scrollback_lines`]`(lines, new_cols)`.
#[must_use]
pub(super) fn reflow_newest_rows(lines: &[Line], new_cols: u16, rows: usize) -> Vec<Line> {
    let mut runs: Vec<Vec<Line>> = Vec::new();
    let mut produced = 0;
    let mut end = lines.len();
    while end > 0 && produced < rows {
        let head = lines[..end]
            .iter()
            .rposition(|line| !line.is_wrapped())
            .unwrap_or(0);
        let run = reflow_scrollback_lines(&lines[head..end], new_cols);
        produced += run.len();
        runs.push(run);
        end = head;
    }
    let mut out: Vec<Line> = runs.into_iter().rev().flatten().collect();
    out.drain(..out.len().saturating_sub(rows));
    out
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
        emit_logical_line(&lines[start..i], new_cols, &mut units, &mut out);
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
) {
    units.clear();
    let first_out = out.len();
    for line in run {
        collect_units(line, units);
        if units.len() >= MAX_LOGICAL_WIDTH {
            break;
        }
    }

    if units.is_empty() {
        out.push(Line::new());
        carry_image_spans(run, new_cols, out, first_out);
        return;
    }

    let cols = new_cols as usize;
    let mut col = 0usize;
    let mut seg_start = 0usize;
    let mut first = true;
    let mut idx = 0usize;
    while idx < units.len() {
        let w = units[idx].width as usize;
        if col + w > cols && col > 0 {
            out.push(build_line(&units[seg_start..idx], !first));
            first = false;
            seg_start = idx;
            col = 0;
        }
        col += w;
        idx += 1;
    }
    out.push(build_line(&units[seg_start..], !first));
    carry_image_spans(run, new_cols, out, first_out);
}

fn carry_image_spans(run: &[Line], new_cols: u16, out: &mut [Line], first_out: usize) {
    let [line] = run else {
        return;
    };
    let Some(spans) = line.images() else {
        return;
    };
    let Some(target) = out.get_mut(first_out) else {
        return;
    };
    let clipped: Vec<ImageSpan> = spans
        .iter()
        .filter(|span| span.start_col < new_cols)
        .map(|span| ImageSpan {
            end_col: span.end_col.min(new_cols),
            ..span.clone()
        })
        .collect();
    target.set_images(clipped);
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
        let link = spans
            .get(span_idx)
            .filter(|s| s.contains(col))
            .map(|s| (s.url.clone(), s.id.clone()));
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
    let mut text = String::with_capacity(
        units
            .iter()
            .map(|unit| unit.text.len())
            .fold(0usize, usize::saturating_add),
    );
    let mut attrs_rle: Rle<CellAttrs> = Rle::new();
    let mut spans: Vec<HyperlinkSpan> = Vec::new();
    let mut open: Option<(u16, std::sync::Arc<str>, Option<std::sync::Arc<str>>)> = None;
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
        match (&open, &unit.link) {
            (None, Some((url, id))) => open = Some((col, url.clone(), id.clone())),
            (Some((_, ourl, oid)), Some((url, id)))
                if std::sync::Arc::ptr_eq(ourl, url) && oid == id => {}
            (Some((start, ourl, oid)), next) => {
                spans.push(HyperlinkSpan::with_id(
                    *start,
                    col,
                    ourl.clone(),
                    oid.clone(),
                ));
                open = next.as_ref().map(|(u, i)| (col, u.clone(), i.clone()));
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
    if let Some((start, url, id)) = open {
        spans.push(HyperlinkSpan::with_id(start, col, url, id));
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
mod tests {
    use super::*;

    fn styled(text: &str, wrapped: bool) -> Line {
        let mut l = Line::from(text);
        l.set_wrapped(wrapped);
        l
    }

    #[test]
    fn rewrap_resplits_each_logical_line_at_the_new_width() {
        type Rows<'a> = &'a [(&'a str, bool)];
        let cases: &[(&str, Rows, u16, Rows)] = &[
            (
                "shrink",
                &[("ABCDEFGHIJ", false)],
                4,
                &[("ABCD", false), ("EFGH", true), ("IJ", true)],
            ),
            (
                "grow merges a run",
                &[("ABCD", false), ("EFGH", true), ("IJ", true)],
                20,
                &[("ABCDEFGHIJ", false)],
            ),
            (
                "hard newlines stay",
                &[("ABC", false), ("DEF", false)],
                20,
                &[("ABC", false), ("DEF", false)],
            ),
            ("blank survives", &[("", false)], 10, &[("", false)]),
            (
                "wide char wraps whole",
                &[("世界", false)],
                3,
                &[("世", false), ("界", true)],
            ),
        ];
        for &(name, input, cols, want) in cases {
            let lines: Vec<Line> = input.iter().map(|&(t, w)| styled(t, w)).collect();
            let got: Vec<(String, bool)> = reflow_scrollback_lines(&lines, cols)
                .iter()
                .map(|l| (l.as_str().unwrap_or("").to_string(), l.is_wrapped()))
                .collect();
            let want: Vec<(String, bool)> = want.iter().map(|&(t, w)| (t.to_string(), w)).collect();
            assert_eq!(got, want, "{name}");
        }

        let original = vec![styled("The quick brown fox jumps", false)];
        let round_trip = reflow_scrollback_lines(&reflow_scrollback_lines(&original, 7), 40);
        assert_eq!(round_trip.len(), 1);
        assert_eq!(round_trip[0].as_str(), Some("The quick brown fox jumps"));
    }

    #[test]
    fn rewrap_keeps_spans_on_their_cells_across_a_split() {
        use std::sync::Arc;
        let mut line = Line::with_hyperlinks(
            "ABCD",
            Rle::new(),
            vec![
                HyperlinkSpan::new(0, 1, Arc::from("a")),
                HyperlinkSpan::new(2, 3, Arc::from("b")),
            ],
        );
        line.set_underline_colors(vec![
            UnderlineColorSpan::new(0, 1, 0x01_FF_00_00),
            UnderlineColorSpan::new(2, 3, 0x02_00_00_03),
        ]);
        let out = reflow_scrollback_lines(&[line], 2);
        let texts: Vec<_> = out.iter().map(|l| l.as_str()).collect();
        assert_eq!(texts, [Some("AB"), Some("CD")]);
        for (row, url, color) in [(0, "a", 0x01_FF_00_00), (1, "b", 0x02_00_00_03)] {
            assert_eq!(out[row].get_hyperlink_span(0).map(|s| &*s.url), Some(url));
            assert_eq!(out[row].get_underline_color(0), Some(color));
            assert_eq!(out[row].get_hyperlink_span(1), None, "row {row} gap");
            assert_eq!(out[row].get_underline_color(1), None, "row {row} gap");
        }
    }

    #[test]
    fn rewrap_preserves_stored_ambiguous_width_with_narrow_control() {
        use crate::CellFlags;

        const AMBIGUOUS: char = '\u{00B7}';
        assert_eq!(aterm_grapheme::char_width(AMBIGUOUS), 1);
        assert_eq!(aterm_grapheme::char_width_cjk(AMBIGUOUS), 2);

        let wide = CellAttrs::new(
            CellAttrs::DEFAULT.fg,
            CellAttrs::DEFAULT.bg,
            CellFlags::WIDE.bits(),
        );
        let wide_line = Line::with_attrs(
            "\u{00B7}Z",
            [wide, CellAttrs::DEFAULT].into_iter().collect(),
        );
        let out = reflow_scrollback_lines(&[wide_line], 2);
        assert_eq!(out.len(), 2, "stored-wide middle dot occupies both columns");
        assert_eq!(out[0].as_str(), Some("\u{00B7}"));
        assert_eq!(out[1].as_str(), Some("Z"));
        assert!(
            CellFlags::from_bits(out[0].get_attr(0).flags).contains(CellFlags::WIDE),
            "rewrap keeps the write-time WIDE authority for later materialization"
        );

        let narrow_line = Line::from("\u{00B7}Z");
        let narrow = reflow_scrollback_lines(&[narrow_line], 2);
        assert_eq!(narrow.len(), 1, "narrow control still fits both glyphs");
        assert_eq!(narrow[0].as_str(), Some("\u{00B7}Z"));
    }

    #[test]
    fn rewrap_vs15_overrides_inconsistent_stored_wide_flag() {
        use crate::CellFlags;

        let stored_wide = CellAttrs::new(
            CellAttrs::DEFAULT.fg,
            CellAttrs::DEFAULT.bg,
            CellFlags::WIDE.bits(),
        );
        let line = Line::with_attrs(
            "\u{231A}\u{FE0E}Z",
            [stored_wide, stored_wide, CellAttrs::DEFAULT]
                .into_iter()
                .collect(),
        );
        let out = reflow_scrollback_lines(&[line], 2);
        assert_eq!(
            out.len(),
            1,
            "VS15 keeps the watch narrow despite WIDE attrs"
        );
        assert_eq!(out[0].as_str(), Some("\u{231A}\u{FE0E}Z"));
    }

    fn assert_semantically_equal(fast: &[Line], reference: &[Line]) {
        assert_eq!(fast.len(), reference.len(), "line counts diverge");
        for (idx, (a, b)) in fast.iter().zip(reference).enumerate() {
            assert_eq!(a.as_str(), b.as_str(), "text diverges at line {idx}");
            assert_eq!(
                a.is_wrapped(),
                b.is_wrapped(),
                "wrap flag diverges at line {idx}"
            );
            let chars = a.as_str().map_or(0, |t| t.chars().count());
            for ci in 0..chars {
                assert_eq!(
                    a.get_attr(ci),
                    b.get_attr(ci),
                    "attrs diverge at line {idx} char {ci}"
                );
            }
            for col in 0..64u16 {
                assert_eq!(
                    a.get_hyperlink_span(col)
                        .map(|s| (s.url.clone(), s.id.clone())),
                    b.get_hyperlink_span(col)
                        .map(|s| (s.url.clone(), s.id.clone())),
                    "hyperlink diverges at line {idx} col {col}"
                );
                assert_eq!(
                    a.get_underline_color(col),
                    b.get_underline_color(col),
                    "underline colour diverges at line {idx} col {col}"
                );
            }
        }
    }

    #[test]
    fn reflow_passthrough_matches_full_path_on_mixed_corpus() {
        use std::sync::Arc;

        let styled_attr = CellAttrs::new(0x01_11_22_33, CellAttrs::DEFAULT.bg, 0);
        let wide_attr = CellAttrs::new(
            CellAttrs::DEFAULT.fg,
            CellAttrs::DEFAULT.bg,
            crate::CellFlags::WIDE.bits(),
        );
        let mut corpus: Vec<Line> = Vec::new();
        for i in 0..40 {
            corpus.push(styled(&format!("ls -la {i}"), false));
            corpus.push(Line::with_attrs(
                "AB",
                [styled_attr, CellAttrs::DEFAULT].into_iter().collect(),
            ));
            corpus.push(styled("", false));
            corpus.push(styled("ABCDEFGH", false));
            corpus.push(styled("IJKLMNOP", true));
            corpus.push(styled("世界世界", false));
            corpus.push(Line::with_attrs(
                "\u{231A}\u{FE0E}Z",
                [wide_attr, wide_attr, CellAttrs::DEFAULT]
                    .into_iter()
                    .collect(),
            ));
            corpus.push(Line::with_attrs(
                "WZ",
                [wide_attr, CellAttrs::DEFAULT].into_iter().collect(),
            ));
            corpus.push(Line::with_hyperlinks(
                "ABCD",
                Rle::new(),
                vec![
                    HyperlinkSpan::new(0, 2, Arc::from("u0")),
                    HyperlinkSpan::new(2, 4, Arc::from("u1")),
                ],
            ));
            let mut ul = Line::with_hyperlinks("ABCD", Rle::new(), Vec::new());
            ul.set_underline_colors(vec![UnderlineColorSpan::new(0, 2, 0x01_FF_00_00)]);
            corpus.push(ul);
            corpus.push(styled(&format!("L{i}-{}", "x".repeat(50)), false));
        }

        for new_cols in [11u16, 23, 40, 200] {
            let _ = crate::test_counters::take_reflow_passthrough_lines();
            let fast = reflow_scrollback_lines(&corpus, new_cols);
            let fired = crate::test_counters::take_reflow_passthrough_lines();
            let reference = reflow_scrollback_lines_reference(&corpus, new_cols);
            assert_semantically_equal(&fast, &reference);
            assert!(
                fired >= 80,
                "passthrough must fire on the eligible majority at {new_cols} \
                 cols (fired {fired})"
            );
        }
    }

    #[test]
    fn reflow_passthrough_never_fires_on_wrap_changing_input() {
        let _ = crate::test_counters::take_reflow_passthrough_lines();
        let long: Vec<Line> = (0..10)
            .map(|i| styled(&format!("L{i}-{}", "x".repeat(60)), false))
            .collect();
        let out = reflow_scrollback_lines(&long, 40);
        assert!(out.len() > 10, "sanity: the long lines actually wrapped");
        assert_eq!(
            crate::test_counters::take_reflow_passthrough_lines(),
            0,
            "no passthrough when the wrap points change"
        );
        let cjk = vec![styled("世界", false)];
        let _ = reflow_scrollback_lines(&cjk, 40);
        assert_eq!(
            crate::test_counters::take_reflow_passthrough_lines(),
            0,
            "no passthrough for non-ASCII content"
        );
        let run = vec![styled("ABC", false), styled("DEF", true)];
        let _ = reflow_scrollback_lines(&run, 40);
        assert_eq!(
            crate::test_counters::take_reflow_passthrough_lines(),
            0,
            "no passthrough for multi-row logical lines"
        );
    }

    #[test]
    fn reflow_newest_rows_is_the_tail_of_the_whole_rewrap() {
        let mut corpus = vec![styled("orphan tail", true)];
        for i in 0..12 {
            corpus.push(styled(&format!("R{i}-ABCDEFGH"), false));
            corpus.push(styled("IJKLMNOP", true));
            corpus.push(styled("QR", true));
            corpus.push(styled("", false));
            corpus.push(styled("世界世界世", false));
            corpus.push(styled(&format!("short {i}"), false));
        }
        for new_cols in [3u16, 7, 11, 40] {
            let whole = reflow_scrollback_lines(&corpus, new_cols);
            for rows in 0..=whole.len() + 2 {
                let newest = reflow_newest_rows(&corpus, new_cols, rows);
                assert_semantically_equal(&newest, &whole[whole.len().saturating_sub(rows)..]);
            }
        }

        let plain: Vec<Line> = (0..1000).map(|i| styled(&format!("p{i}"), false)).collect();
        let _ = crate::test_counters::take_reflow_passthrough_lines();
        let newest = reflow_newest_rows(&plain, 40, 1);
        assert_eq!(newest.len(), 1);
        assert_eq!(newest[0].as_str(), Some("p999"));
        assert_eq!(
            crate::test_counters::take_reflow_passthrough_lines(),
            1,
            "one row asked of single-row lines rewraps the newest one only"
        );
    }
}
