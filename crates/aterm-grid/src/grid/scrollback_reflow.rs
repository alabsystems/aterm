// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Off-screen scrollback reflow: rewrap history [`Line`]s on a width change.
//!
//! The visible-grid reflow in [`super::reflow`] only rewraps the on-screen
//! rows; the off-screen scrollback (tiered storage, lazy buffer, and ring-
//! buffer scrollback rows) lives outside that window. Without this step a
//! resize would silently drop ALL history (#7906). This rewraps the full
//! history at the logical-line level — joining soft-wrapped runs, then
//! re-splitting by DISPLAY WIDTH to the new column count — preserving text,
//! per-character attributes, and hyperlinks in both directions.

use aterm_rle::Rle;
use aterm_scrollback::{CellAttrs, HyperlinkSpan, Line, UnderlineColorSpan};

use super::Grid;

impl Grid {
    /// Extract the ENTIRE off-screen scrollback as logical [`Line`]s (oldest
    /// first) and remove it from the grid, leaving only the visible rows.
    ///
    /// Order matches `get_history_line`: tiered scrollback, then the lazy
    /// buffer, then ring-buffer scrollback. After this returns the grid has no
    /// scrollback (tiered cleared, lazy drained, ring scrollback rows dropped),
    /// so the visible-grid reflow runs against a clean history that this
    /// function's rewrapped output is later prepended to.
    // COST: UNBOUNDED(ring+tiered-history-lines) — materializes the ENTIRE
    // off-screen scrollback. See `xtask gate mainloop` (MAIN-LOOP COMPLETENESS
    // CENSUS): must stay behind the `resize_offloading_scrollback` detach on any
    // main-thread-reachable path.
    pub(super) fn take_scrollback_lines(&mut self) -> Vec<Line> {
        // Tiered + lazy: materialize then clear. drain_lazy_buffer pushes lazy
        // lines into tiered, so a single tiered sweep then covers both.
        self.drain_lazy_buffer();
        let mut lines = Vec::new();
        if let Some(scrollback) = self.storage.scrollback.as_mut() {
            let count = scrollback.line_count();
            lines.reserve(count);
            for i in 0..count {
                match scrollback.get_line(i) {
                    Ok(Some(line)) => lines.push(line.into_owned()),
                    // A decode failure must not silently truncate older history:
                    // keep a blank placeholder so indices/ordering stay sane.
                    Ok(None) | Err(_) => lines.push(Line::new()),
                }
            }
            if let Err(error) = scrollback.clear() {
                aterm_log::warn!("scrollback clear during reflow failed: {error}");
            }
        }

        // Ring-buffer scrollback: the rows preceding the visible window. The
        // caller has already drained the lazy buffer (above), so all deferred
        // lines are accounted for. Linearize so logical order == Vec order.
        let ring_scrollback = self.storage.ring_buffer_scrollback();
        if ring_scrollback > 0 {
            let ring_head = self.storage.ring_head;
            if ring_head != 0 {
                self.storage.rows.rotate_left(ring_head);
                self.storage.ring_head = 0;
            }
            lines.reserve(ring_scrollback);
            for i in 0..ring_scrollback {
                let extras = self
                    .storage
                    .ring_history_extras(i)
                    .cloned()
                    .unwrap_or_default();
                lines.push(Self::row_to_line_with_stored_extras(
                    &self.storage.rows[i],
                    &extras,
                ));
            }
            // Drop the scrollback rows; keep only the visible window.
            self.storage.rows.drain(..ring_scrollback);
            self.storage.ring_extras.clear();
            self.storage.total_lines = self.storage.rows.len();
        }

        lines
    }

    /// Push rewrapped scrollback [`Line`]s back into history as the FRONT (oldest)
    /// of the scrollback, ahead of any overflow the visible-grid reflow produced.
    ///
    /// Lines go straight into tiered scrollback when it is attached (the
    /// normal path; `push_line` honors its line limit), otherwise they are
    /// converted to ring-buffer scrollback rows up to the configured
    /// `max_scrollback` cap (older lines beyond the cap are evicted —
    /// the correct, configured behavior).
    pub(super) fn restore_reflowed_scrollback(&mut self, lines: Vec<Line>, new_cols: u16) {
        if lines.is_empty() {
            return;
        }
        if let Some(scrollback) = self.storage.scrollback.as_mut() {
            // Tiered storage was cleared by take_scrollback_lines and the lazy
            // buffer holds only the visible-grid reflow's overflow, so pushing
            // the (older) rewrapped history directly, then draining, keeps the
            // [old scrollback | reflow overflow] order without staging each
            // Line through a DeferredLine clone.
            for line in lines {
                if let Err(error) = scrollback.push_line(line) {
                    aterm_log::warn!("scrollback push_line during reflow failed: {error}");
                }
            }
            if self.storage.lazy_buffer.is_empty() {
                // No overflow pending: drain_lazy_buffer would early-return,
                // but budget enforcement + display-offset clamping (#7240)
                // must still run after the direct pushes above.
                self.enforce_scrollback_budget_and_clamp();
            } else {
                self.drain_lazy_buffer();
            }
        } else {
            self.prepend_ring_scrollback_lines(lines, new_cols);
        }
    }

    /// Convert reflowed scrollback [`Line`]s into ring-buffer scrollback rows
    /// and prepend them ahead of the visible window, honoring `max_scrollback`.
    fn prepend_ring_scrollback_lines(&mut self, lines: Vec<Line>, new_cols: u16) {
        // Cap to the ring's scrollback budget: only the newest lines fit when
        // history exceeds the configured limit (oldest evicted — correct).
        let cap = self.storage.max_scrollback;
        let skip = lines.len().saturating_sub(cap);
        // During an off-thread reflow the tiered store is detached, so this overflow
        // can't spill to disk-tier — it would be lost where a non-detached grid would
        // have kept it. Stage it to the lazy buffer (flushed on re-attach) instead of
        // dropping it (audit #3). These reflowed ring lines are NEWER than any
        // existing lazy content (older window output), so push at the back; the
        // (still newer) `kept` lines below become ring scrollback ahead of them.
        if skip > 0 && self.storage.scrollback_detached_for_reflow {
            for line in &lines[..skip] {
                let (row, extras) = self.build_scrollback_row(line, new_cols);
                self.storage.lazy_buffer.push_row(&row, extras);
            }
        }
        let kept = &lines[skip..];
        if kept.is_empty() {
            return;
        }

        // Linearize so we can splice scrollback rows at the front (index 0).
        let ring_head = self.storage.ring_head;
        if ring_head != 0 {
            self.storage.rows.rotate_left(ring_head);
            self.storage.ring_head = 0;
        }

        // Build the scrollback rows at the new width, capturing extras keyed by
        // their ring-scrollback index for ring_extras (front = oldest).
        let mut new_rows = Vec::with_capacity(kept.len());
        let mut new_extras = Vec::with_capacity(kept.len());
        for line in kept {
            let (row, extras) = self.build_scrollback_row(line, new_cols);
            new_rows.push(row);
            new_extras.push(if extras.is_empty() {
                None
            } else {
                Some(Box::new(extras))
            });
        }

        let added = new_rows.len();
        // Splice the scrollback rows in front of the (linearized) visible rows.
        let visible: Vec<_> = std::mem::take(&mut self.storage.rows);
        new_rows.extend(visible);
        self.storage.rows = new_rows;
        for (i, extra) in new_extras.into_iter().enumerate() {
            self.storage.ring_extras.insert(i, extra);
        }
        self.storage.total_lines = self.storage.rows.len();
        self.storage.absolute_row_counter = self
            .storage
            .absolute_row_counter
            .saturating_add(added as u64);
    }

    /// Build a single scrollback [`Row`](crate::Row) plus its preserved extras
    /// from a [`Line`] at the new width, reusing the unscroll fill path.
    fn build_scrollback_row(
        &mut self,
        line: &Line,
        new_cols: u16,
    ) -> (crate::Row, super::ScrolledRowExtras) {
        // SAFETY: the row is moved into `self.storage.rows` by the caller, which
        // owns `self.storage.pages` for at least as long as the row lives.
        let mut row = unsafe { crate::Row::new(new_cols, &mut self.storage.pages) };
        let extras = super::scroll_fill::fill_row_into(&mut row, line, new_cols, self.styles());
        (row, extras)
    }
}

/// Maximum display columns a logical line may span before we stop accumulating,
/// guarding against pathological inputs (`MAX_GRID_COLS` * a large row count).
const MAX_LOGICAL_WIDTH: usize = crate::MAX_GRID_COLS as usize * crate::MAX_GRID_ROWS as usize;

/// One display-cell's worth of content extracted from a source [`Line`].
struct Unit<'a> {
    /// The grapheme text for the cell (base char plus combining marks / ZWJ).
    text: &'a str,
    /// Attributes for the cell.
    attrs: CellAttrs,
    /// Hyperlink (url, id) covering this cell, if any.
    link: Option<(std::sync::Arc<str>, Option<std::sync::Arc<str>>)>,
    /// Packed SGR 58 underline colour covering this cell, if any.
    underline_color: Option<u32>,
    /// Display width (1 or 2).
    width: u16,
}

/// Rewrap a sequence of scrollback [`Line`]s to `new_cols`, preserving logical
/// line breaks (hard newlines) and content. Soft-wrapped runs (each line after
/// the first in a run carries the wrapped flag) are joined, then re-split by
/// display width. O(total cells); the display-cell scratch buffer is reused
/// across logical lines, so it allocates once for the widest logical line.
// COST: UNBOUNDED(session-history-cells) — rewraps O(total cells) of history.
// See `xtask gate mainloop` (MAIN-LOOP COMPLETENESS CENSUS): the 42s freeze sink;
// must stay off the main thread (behind `resize_offloading_scrollback`/a worker).
#[must_use]
pub(super) fn reflow_scrollback_lines(lines: &[Line], new_cols: u16) -> Vec<Line> {
    let new_cols = new_cols.max(1);
    let mut out: Vec<Line> = Vec::with_capacity(lines.len());
    // Scratch buffer shared across logical lines (cleared per line): one
    // allocation for the whole history instead of one per logical line.
    let mut units: Vec<Unit<'_>> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        // A logical line = lines[i] (non-wrapped or first) plus following
        // lines whose wrapped flag marks them as soft continuations.
        let start = i;
        i += 1;
        while i < lines.len() && lines[i].is_wrapped() {
            i += 1;
        }
        emit_logical_line(&lines[start..i], new_cols, &mut units, &mut out);
    }
    out
}

/// Flatten a logical-line run into display-cell units (accumulated in the
/// caller-owned `units` scratch buffer), then re-split to `new_cols`-wide
/// output [`Line`]s (first not wrapped, rest wrapped).
fn emit_logical_line<'a>(
    run: &'a [Line],
    new_cols: u16,
    units: &mut Vec<Unit<'a>>,
    out: &mut Vec<Line>,
) {
    units.clear();
    for line in run {
        collect_units(line, units);
        if units.len() >= MAX_LOGICAL_WIDTH {
            break;
        }
    }

    if units.is_empty() {
        // Preserve a blank logical line (e.g. an empty hard-newline row).
        out.push(Line::new());
        return;
    }

    let cols = new_cols as usize;
    let mut col = 0usize;
    let mut seg_start = 0usize;
    let mut first = true;
    let mut idx = 0usize;
    while idx < units.len() {
        let w = units[idx].width as usize;
        // A wide char that would straddle the right edge wraps to the next row.
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
}

/// Decompose a [`Line`] into per-display-cell units (text + attrs + hyperlink).
fn collect_units<'a>(line: &'a Line, units: &mut Vec<Unit<'a>>) {
    let Some(text) = line.as_str() else {
        return;
    };
    let mut byte_idx = 0usize;
    let mut char_idx = 0usize;
    let mut col: u16 = 0;
    // Hyperlink spans are emitted sorted by start_col and disjoint (every producer
    // walks columns left-to-right), and `col` below is monotonic, so resolve each
    // cell's link with a single advancing cursor instead of Line::get_hyperlink_span's
    // linear `.find()`. The per-cell find made collect_units O(cells × spans) — a
    // quadratic resize-time hang on a scrollback line carrying a distinct OSC-8 URL
    // per cell (round-9). Mirrors the advancing-cursor pattern in scroll_convert.rs.
    let spans = line.hyperlinks().unwrap_or(&[]);
    debug_assert!(
        spans.windows(2).all(|w| w[0].start_col <= w[1].start_col),
        "hyperlink spans must be sorted by start_col for the cursor scan"
    );
    let mut span_idx = 0usize;
    // Underline-colour spans use the same sorted+disjoint advancing-cursor scan.
    let ul_spans = line.underline_colors().unwrap_or(&[]);
    debug_assert!(
        ul_spans
            .windows(2)
            .all(|w| w[0].start_col <= w[1].start_col),
        "underline-colour spans must be sorted by start_col for the cursor scan"
    );
    let mut ul_idx = 0usize;
    while byte_idx < text.len() {
        let c = text[byte_idx..]
            .chars()
            .next()
            .expect("invariant: byte_idx < text.len()");
        let base_width = aterm_grapheme::char_width(c);
        if base_width == 0 {
            // Orphan zero-width char with no base; skip (matches materialize).
            byte_idx += c.len_utf8();
            char_idx += 1;
            continue;
        }
        let unit_byte_start = byte_idx;
        let unit_char_start = char_idx;
        // Effective width: replays the live VS16/VS15 presentation-selector
        // transitions (see `advance_grapheme_unit_wide`) so reflowed rows keep
        // the exact columns the live grid used (`❤️` two, `⌚︎` one). No row-edge
        // demotion here: reflow runs only on an actual width CHANGE
        // (`reflow.rs` gates on `new_cols != old_cols`), where a VS16 unit that
        // was edge-pinned narrow legitimately regains its 2-cell emoji
        // presentation at its new position — exactly what a live rewrite would do.
        let unit = super::scroll_materialize::advance_grapheme_unit_wide(text, &mut byte_idx);
        let chars_consumed = unit.chars;
        char_idx += chars_consumed;
        let attrs = line.get_attr(unit_char_start);
        let width = if super::scroll_materialize::stored_unit_is_wide(unit, attrs) {
            2
        } else {
            1
        };
        // Advance past spans fully left of `col`; sorted + disjoint ⇒ at most one
        // remaining span can contain `col`. O(cells + spans) overall.
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
            text: &text[unit_byte_start..byte_idx],
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

/// Build an output [`Line`] from a slice of display-cell units.
fn build_line(units: &[Unit<'_>], wrapped: bool) -> Line {
    let mut text = String::new();
    let mut attrs_rle: Rle<CellAttrs> = Rle::new();
    let mut spans: Vec<HyperlinkSpan> = Vec::new();
    // Coalesce consecutive cells sharing a hyperlink (url ptr + id) into spans.
    let mut open: Option<(u16, std::sync::Arc<str>, Option<std::sync::Arc<str>>)> = None;
    // Same coalescing for SGR 58 underline colours (packed u32, so by value).
    let mut ul_spans: Vec<UnderlineColorSpan> = Vec::new();
    let mut ul_open: Option<(u16, u32)> = None;
    let mut col: u16 = 0;

    for unit in units {
        let char_count = unit.text.chars().count();
        text.push_str(unit.text);
        for _ in 0..char_count {
            attrs_rle.push(unit.attrs);
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

    let mut line = Line::with_hyperlinks(&text, attrs_rle, spans);
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
    fn rewrap_preserves_disjoint_hyperlinks_across_a_split() {
        // Round-9: collect_units resolves per-cell links with an advancing cursor over
        // the sorted/disjoint span list (was a per-cell linear scan → quadratic). Two
        // alternating-URL spans (which defeat coalescing) must each still map to the
        // right cells after a width-2 split re-buckets them into separate output lines.
        use std::sync::Arc;
        let spans = vec![
            HyperlinkSpan::new(0, 2, Arc::from("u0")),
            HyperlinkSpan::new(2, 4, Arc::from("u1")),
        ];
        let line = Line::with_hyperlinks("ABCD", Rle::new(), spans);
        let out = reflow_scrollback_lines(&[line], 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].as_str().unwrap(), "AB");
        assert_eq!(out[1].as_str().unwrap(), "CD");
        assert_eq!(out[0].get_hyperlink_span(0).map(|s| &*s.url), Some("u0"));
        assert_eq!(out[0].get_hyperlink_span(1).map(|s| &*s.url), Some("u0"));
        assert_eq!(out[1].get_hyperlink_span(0).map(|s| &*s.url), Some("u1"));
        assert_eq!(out[1].get_hyperlink_span(1).map(|s| &*s.url), Some("u1"));
    }

    #[test]
    fn rewrap_preserves_underline_colors_across_a_split() {
        // Underline colours must survive a width-change reflow (the third
        // Line-producing path). Two differently-coloured runs, split across the
        // wrap, must each land the right packed colour on the right cells.
        let mut line = Line::with_hyperlinks("ABCD", Rle::new(), Vec::new());
        line.set_underline_colors(vec![
            UnderlineColorSpan::new(0, 2, 0x01_FF_00_00), // RGB red, cols 0-1
            UnderlineColorSpan::new(2, 4, 0x02_00_00_03), // indexed 3, cols 2-3
        ]);
        let out = reflow_scrollback_lines(&[line], 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].as_str().unwrap(), "AB");
        assert_eq!(out[1].as_str().unwrap(), "CD");
        assert_eq!(out[0].get_underline_color(0), Some(0x01_FF_00_00));
        assert_eq!(out[0].get_underline_color(1), Some(0x01_FF_00_00));
        assert_eq!(out[1].get_underline_color(0), Some(0x02_00_00_03));
        assert_eq!(out[1].get_underline_color(1), Some(0x02_00_00_03));
    }

    #[test]
    fn rewrap_underline_color_gap_cell_has_none() {
        // A cell in the GAP between two underline-colour spans must carry no
        // colour after reflow (advancing cursor + contains() rejection).
        let mut line = Line::with_hyperlinks("ABC", Rle::new(), Vec::new());
        line.set_underline_colors(vec![
            UnderlineColorSpan::new(0, 1, 0x01_11_22_33),
            UnderlineColorSpan::new(2, 3, 0x01_44_55_66),
        ]);
        let out = reflow_scrollback_lines(&[line], 20);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].get_underline_color(0), Some(0x01_11_22_33));
        assert_eq!(
            out[0].get_underline_color(1),
            None,
            "gap cell must carry no colour"
        );
        assert_eq!(out[0].get_underline_color(2), Some(0x01_44_55_66));
    }

    #[test]
    fn rewrap_hyperlink_gap_cell_has_no_link() {
        // The cursor must return None for a cell that falls in the GAP between two
        // spans (advancing to the next span whose end_col > col, then rejecting it via
        // contains()). A naive cursor that dropped the contains() check would wrongly
        // attribute the gap cell to the following span.
        use std::sync::Arc;
        let spans = vec![
            HyperlinkSpan::new(0, 1, Arc::from("a")),
            HyperlinkSpan::new(2, 3, Arc::from("b")),
        ];
        let line = Line::with_hyperlinks("ABC", Rle::new(), spans);
        let out = reflow_scrollback_lines(&[line], 20);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].get_hyperlink_span(0).map(|s| &*s.url), Some("a"));
        assert_eq!(
            out[0].get_hyperlink_span(1),
            None,
            "gap cell must carry no link"
        );
        assert_eq!(out[0].get_hyperlink_span(2).map(|s| &*s.url), Some("b"));
    }

    #[test]
    fn rewrap_shrink_splits_logical_line() {
        // One logical line "ABCDEFGHIJ" at width 10 -> width 4 = 3 rows.
        let lines = vec![styled("ABCDEFGHIJ", false)];
        let out = reflow_scrollback_lines(&lines, 4);
        let texts: Vec<_> = out
            .iter()
            .map(|l| l.as_str().unwrap().to_string())
            .collect();
        assert_eq!(texts, vec!["ABCD", "EFGH", "IJ"]);
        assert!(!out[0].is_wrapped());
        assert!(out[1].is_wrapped());
        assert!(out[2].is_wrapped());
    }

    #[test]
    fn rewrap_grow_merges_soft_wrapped_run() {
        // "ABCD" + wrapped "EFGH" + wrapped "IJ" -> width 20 = one row.
        let lines = vec![
            styled("ABCD", false),
            styled("EFGH", true),
            styled("IJ", true),
        ];
        let out = reflow_scrollback_lines(&lines, 20);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].as_str().unwrap(), "ABCDEFGHIJ");
        assert!(!out[0].is_wrapped());
    }

    #[test]
    fn rewrap_preserves_hard_newlines() {
        let lines = vec![styled("ABC", false), styled("DEF", false)];
        let out = reflow_scrollback_lines(&lines, 20);
        let texts: Vec<_> = out
            .iter()
            .map(|l| l.as_str().unwrap().to_string())
            .collect();
        assert_eq!(texts, vec!["ABC", "DEF"]);
        assert!(!out[0].is_wrapped());
        assert!(!out[1].is_wrapped());
    }

    #[test]
    fn rewrap_round_trip_is_content_stable() {
        let original = vec![styled("The quick brown fox jumps", false)];
        let narrow = reflow_scrollback_lines(&original, 7);
        let wide = reflow_scrollback_lines(&narrow, 40);
        assert_eq!(wide.len(), 1);
        assert_eq!(wide[0].as_str().unwrap(), "The quick brown fox jumps");
    }

    #[test]
    fn rewrap_blank_logical_line_survives() {
        let lines = vec![styled("", false)];
        let out = reflow_scrollback_lines(&lines, 10);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].as_str().unwrap_or(""), "");
    }

    #[test]
    fn rewrap_wide_char_not_split_across_rows() {
        // Two wide chars (width 2 each) + width 3 = needs >= 4 cols to hold one.
        let lines = vec![styled("世界", false)];
        let out = reflow_scrollback_lines(&lines, 3);
        // width 3: first wide char fits (2), second would straddle -> wraps.
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].as_str().unwrap(), "世");
        assert_eq!(out[1].as_str().unwrap(), "界");
        assert!(out[1].is_wrapped());
    }

    #[test]
    fn rewrap_preserves_stored_ambiguous_width_with_narrow_control() {
        use crate::CellFlags;

        const AMBIGUOUS: char = '\u{00B7}'; // MIDDLE DOT, East Asian Width=A
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
}
