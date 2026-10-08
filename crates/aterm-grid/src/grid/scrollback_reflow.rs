// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Off-screen scrollback reflow: rewrap history [`Line`]s on a width change.
//!
//! The visible-grid reflow in [`super::reflow`] rewraps only on-screen rows;
//! the off-screen scrollback (tiered storage, lazy buffer, ring scrollback
//! rows) is rewrapped here at the logical-line level: soft-wrapped runs are
//! joined, then re-split by display width to the new column count, keeping
//! text, per-character attributes and hyperlinks in both directions.

use aterm_rle::Rle;
use aterm_scrollback::{CellAttrs, HyperlinkSpan, ImageSpan, Line, UnderlineColorSpan};

use super::Grid;

impl Grid {
    /// Extract the entire off-screen scrollback as logical [`Line`]s (oldest
    /// first) and remove it from the grid, leaving only the visible rows.
    ///
    /// Order matches `get_history_line`: tiered scrollback, then the lazy
    /// buffer, then ring-buffer scrollback. Afterwards the grid has no
    /// scrollback, so the visible-grid reflow runs against a clean history that
    /// this function's rewrapped output is later prepended to.
    // COST: UNBOUNDED(ring+tiered-history-lines) — materializes the entire
    // off-screen scrollback. Must stay behind the `resize_offloading_scrollback`
    // detach on any main-thread-reachable path (`aterm-census --mainloop`).
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
        // lines are accounted for.
        lines.extend(self.take_ring_scrollback_lines());

        lines
    }

    /// Extract only the ring-buffer scrollback (the rows preceding the visible
    /// window) as logical [`Line`]s (oldest first), removing those rows from
    /// the ring. Ring history extras ride inside the produced [`Line`]s;
    /// `ring_extras` is cleared.
    ///
    /// Separate so the offloaded width-change path
    /// (`resize_offloading_scrollback`) can hand the ring history to the
    /// detached job: the synchronous work is then this one materialize pass,
    /// with the rewrap in the job's ring phase and the row rebuild at
    /// re-attach.
    // COST: O(ring-scrollback-rows): bounded by the ring capacity (a
    // construction-time constant), not by session history.
    pub(super) fn take_ring_scrollback_lines(&mut self) -> Vec<Line> {
        let ring_scrollback = self.storage.ring_buffer_scrollback();
        if ring_scrollback == 0 {
            return Vec::new();
        }
        // Linearize so logical order == Vec order.
        let ring_head = self.storage.ring_head;
        if ring_head != 0 {
            self.storage.rows.rotate_left(ring_head);
            self.storage.ring_head = 0;
        }
        let mut lines = Vec::with_capacity(ring_scrollback);
        // Borrow the stored extras instead of cloning them (`ring_extras` and
        // `rows` are disjoint fields), avoiding several allocations per ring row
        // under the `term` lock. One empty default covers the `None` rows, as in
        // `try_get_history_line`. Declared after the `rotate_left` above so the
        // shared borrows never overlap the `&mut`.
        let no_extras = super::ScrolledRowExtras::default();
        for i in 0..ring_scrollback {
            let extras = self.storage.ring_history_extras(i).unwrap_or(&no_extras);
            let row = &self.storage.rows[i];
            // A row whose logical line continues (its successor, the next ring
            // row or the first visible row, is a wrap continuation) was filled
            // to its last column by autowrap, so its trailing blanks are
            // content: materialize the full width, or a width sweep erodes one
            // space per chunk boundary. Except when the continuation opens with
            // a wide cell: a wide char that cannot start at the last column
            // wraps early, leaving that cell unwritten.
            let successor = self.storage.rows.get(i + 1);
            let len = if row.line_size() == super::LineSize::SingleWidth
                && successor.is_some_and(super::Row::is_wrapped)
            {
                // Autowrap filled this row, so its trailing blanks are content.
                // A successor opening wide means exactly one cell (the
                // early-wrap hole) was never written.
                let hole = successor
                    .and_then(|r| r.as_slice().first())
                    .is_some_and(super::Cell::is_wide);
                row.cols() - u16::from(hole)
            } else {
                row.len()
            };
            lines.push(Self::row_to_line_with_stored_extras_at_len(
                row, extras, len,
            ));
        }
        // Drop the scrollback rows; keep only the visible window.
        self.storage.recycle_rows(0..ring_scrollback);
        self.storage.ring_extras.clear();
        self.storage.total_lines = self.storage.rows.len();
        lines
    }

    /// Push rewrapped scrollback [`Line`]s back into history as the front
    /// (oldest) of the scrollback, ahead of any overflow the visible-grid
    /// reflow produced.
    ///
    /// Lines go straight into tiered scrollback when attached (`push_line`
    /// honors its line limit), otherwise they become ring-buffer scrollback
    /// rows up to `max_scrollback` (older lines beyond the cap are evicted).
    pub(super) fn restore_reflowed_scrollback(&mut self, mut lines: Vec<Line>, new_cols: u16) {
        if lines.is_empty() && self.storage.scrollback.is_some() {
            return;
        }
        if let Some(scrollback) = self.storage.scrollback.as_mut() {
            // Tiered storage was cleared by take_scrollback_lines and the lazy
            // buffer holds only the visible reflow's overflow, so pushing the
            // older rewrapped history directly, then draining, keeps
            // [old scrollback | reflow overflow] order without staging each
            // Line.
            for line in lines {
                if let Err(error) = scrollback.push_line(line) {
                    aterm_log::warn!("scrollback push_line during reflow failed: {error}");
                }
            }
            if self.storage.lazy_buffer.is_empty() {
                // No overflow pending: drain_lazy_buffer would early-return, but
                // budget enforcement and display-offset clamping must still run.
                self.enforce_scrollback_budget_and_clamp();
            } else {
                self.drain_lazy_buffer();
            }
        } else {
            // Ring-only grid: the visible reflow's overflow was staged into the
            // lazy buffer, where with no tiered store the next
            // `drain_lazy_buffer` would discard it, and readers would meanwhile
            // see it in the wrong order (it is newer than `lines`). Absorb it
            // here as the newest tail of the restored history. A detached window
            // keeps its staged output for re-attach instead.
            if !self.storage.scrollback_detached_for_reflow && !self.storage.lazy_buffer.is_empty()
            {
                lines.extend(self.storage.lazy_buffer.drain_all());
            }
            if !lines.is_empty() {
                self.prepend_ring_scrollback_lines(lines, new_cols);
            }
        }
    }

    /// Convert reflowed scrollback [`Line`]s into ring-buffer scrollback rows
    /// and prepend them ahead of the visible window, honoring `max_scrollback`.
    ///
    /// `pub(super)` because the offload re-attach
    /// (`Grid::reattach_ring_history`) rebuilds ring history through this same
    /// path.
    pub(super) fn prepend_ring_scrollback_lines(&mut self, lines: Vec<Line>, new_cols: u16) {
        // Cap to the ring's scrollback budget: only the newest lines fit when
        // history exceeds the configured limit (oldest evicted — correct).
        let cap = self.storage.max_scrollback;
        let skip = lines.len().saturating_sub(cap);
        // During an off-thread reflow the tiered store is detached, so this
        // overflow cannot spill to it; stage it to the lazy buffer (flushed on
        // re-attach) instead of dropping it. These lines are newer than existing
        // lazy content, so push at the back; the still newer `kept` lines below
        // become ring scrollback.
        if skip > 0 && self.storage.scrollback_detached_for_reflow {
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
        // A width rewrap changes how many rows the same history occupies, so
        // every absolute row number above the splice shifts: a wholesale
        // renumbering, which `history_renumber_epoch` signals so
        // absolute-row-keyed caches (the search index, the viewport row cache)
        // rebuild. As with the Kitty unscroll (`scroll_unscroll.rs`), a spurious
        // rebuild is harmless and a missed one is silently wrong.
        self.storage.history_renumber_epoch = self.storage.history_renumber_epoch.saturating_add(1);
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
/// the first carries the wrapped flag) are joined, then re-split by display
/// width. O(total cells) worst case, but single-row logical lines whose wrap
/// points cannot change at `new_cols` pass through as clones
/// (`rewrap_passthrough_eligible`), so typical history costs O(affected cells
/// + clones). The display-cell scratch buffer is reused across logical lines.
// COST: UNBOUNDED(session-history-cells) — rewraps O(total cells) of history.
// Must stay off the main thread, behind `resize_offloading_scrollback` or a
// worker (`aterm-census --mainloop`).
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
        let run = &lines[start..i];
        // Passthrough: a single-row logical line whose wrap points cannot change
        // at `new_cols` is re-emitted as a clone, skipping decomposition and
        // rebuild. The gate is conservative (`rewrap_passthrough_eligible`); the
        // `reflow_passthrough_*` differential tests compare against the full
        // path, and the counter checks the fast path fires on eligible corpora
        // and never on wrap-changing ones.
        if run.len() == 1 && rewrap_passthrough_eligible(&run[0], new_cols) {
            #[cfg(any(test, feature = "testing"))]
            super::count_reflow_passthrough_lines(1);
            out.push(run[0].cloned_for_rewrap());
            continue;
        }
        emit_logical_line(run, new_cols, &mut units, &mut out);
    }
    out
}

/// The full-decomposition rewrap with passthrough disabled: the reference for
/// the passthrough differential tests. Test-only; shares `emit_logical_line`
/// with the shipping path.
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

/// Wrap-invariance gate for the clone-through fast path: true only when every
/// cell's display width is 1, the total fits `new_cols`, and no span sidecar
/// the rebuild re-derives is present:
///
///   * single-row unwrapped logical line (checked by the caller; a leading
///     orphaned continuation is fine, since `cloned_for_rewrap` resets the
///     flags as `build_line` would),
///   * content is printable ASCII (0x20..=0x7E): no combining marks,
///     presentation selectors, zero-width or wide chars, so byte length ==
///     display width,
///   * that length fits `new_cols`,
///   * no stored WIDE attr flag (`stored_unit_is_wide` can double a cell's
///     width even when `char_width` says 1; see
///     `rewrap_preserves_stored_ambiguous_width_with_narrow_control`),
///   * no hyperlink / SGR-58 underline-colour spans (the rebuild may
///     re-coalesce them),
///   * no inline-image spans (a clone would carry a footprint unclipped past
///     the new right margin, while the rebuild clips it, as a live placement
///     does).
///
/// Attribute content needs no gating: the rebuild copies per-cell attrs
/// verbatim, and only the WIDE bit affects geometry.
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
        // Non-UTF-8 content: the rebuild path maps it to a blank line; never
        // clone it through.
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
    // Where this logical line's output starts, so its inline images can be
    // re-attached to the segment keeping their columns (`carry_image_spans`).
    let first_out = out.len();
    for line in run {
        collect_units(line, units);
        if units.len() >= MAX_LOGICAL_WIDTH {
            break;
        }
    }

    if units.is_empty() {
        // Preserve a blank logical line (e.g. an empty hard-newline row), which
        // is also what an image row is: `place_image` writes no glyph, so its
        // content is only the image spans carried below.
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
    carry_image_spans(run, new_cols, out, first_out);
}

/// Re-attach a logical line's inline-image spans to the rewrapped output,
/// clipped to the new width.
///
/// The first output segment keeps logical columns `[0, new_cols)`, where the
/// image was anchored, so the spans go there with the end column clipped, as
/// the live grid clips tail tiles at the right margin of a narrower window.
///
/// A multi-line logical run (a soft-wrapped paragraph that also carries a
/// picture) drops its continuation lines' images: their columns are
/// re-bucketed, so no segment describes the footprint. `place_image` line-feeds
/// between footprint rows, so this arises only when an image is stamped onto
/// a row whose text then soft-wraps.
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

/// Decompose a [`Line`] into per-display-cell units (text + attrs + hyperlink).
fn collect_units<'a>(line: &'a Line, units: &mut Vec<Unit<'a>>) {
    let Some(text) = line.as_str() else {
        return;
    };
    let mut byte_idx = 0usize;
    let mut char_idx = 0usize;
    let mut col: u16 = 0;
    // `unit_char_start` is monotone across this walk, so a run cursor reads the
    // RLE attrs in O(runs) total instead of rescanning per cell. This walk
    // covers all retained history on every width change.
    let mut attr_cursor = line.attr_cursor();
    // Hyperlink spans are sorted by start_col and disjoint, and `col` below is
    // monotonic, so resolve each cell's link with one advancing cursor instead
    // of `Line::get_hyperlink_span`'s linear `.find()`, which would be
    // O(cells × spans) on a line with a distinct OSC-8 URL per cell.
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
        let c = super::scroll_materialize::char_at(text, byte_idx);
        if super::scroll_materialize::is_zero_width(c) {
            // Orphan zero-width char with no base; skip (matches materialize).
            byte_idx += c.len_utf8();
            char_idx += 1;
            continue;
        }
        let unit_byte_start = byte_idx;
        let unit_char_start = char_idx;
        // Effective width, replaying the live VS16/VS15 transitions (see
        // `advance_grapheme_unit_wide`) so reflowed rows keep the live columns
        // (`❤️` two, `⌚︎` one). No row-edge demotion: reflow runs only on a real
        // width change, where an edge-pinned VS16 unit regains its 2-cell
        // presentation at its new position, as a live rewrite would.
        let unit = super::scroll_materialize::advance_grapheme_unit_wide(text, &mut byte_idx);
        let chars_consumed = unit.chars;
        char_idx += chars_consumed;
        let attrs = attr_cursor.attr_at(unit_char_start);
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
    // Exact capacity: `text` is exactly the units' text concatenated and is
    // moved into the `Line`, so it is allocated once with no slack or copy.
    let mut text = String::with_capacity(
        units
            .iter()
            .map(|unit| unit.text.len())
            .fold(0usize, usize::saturating_add),
    );
    let mut attrs_rle: Rle<CellAttrs> = Rle::new();
    let mut spans: Vec<HyperlinkSpan> = Vec::new();
    // Coalesce consecutive cells sharing a hyperlink (url ptr + id) into spans.
    let mut open: Option<(u16, std::sync::Arc<str>, Option<std::sync::Arc<str>>)> = None;
    // Same coalescing for SGR 58 underline colours (packed u32, so by value).
    let mut ul_spans: Vec<UnderlineColorSpan> = Vec::new();
    let mut ul_open: Option<(u16, u32)> = None;
    let mut col: u16 = 0;

    for unit in units {
        // A one-byte unit (ASCII, most history) is one char.
        let char_count = if unit.text.len() == 1 {
            1
        } else {
            unit.text.chars().count()
        };
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
    fn rewrap_preserves_disjoint_hyperlinks_across_a_split() {
        // Two alternating-URL spans (which defeat coalescing) must each map to
        // the right cells after a width-2 split puts them in separate lines.
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
        // Underline colours survive a width-change reflow: two differently
        // coloured runs split across the wrap each land on the right cells.
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
        // The cursor returns None for a cell in the gap between two spans
        // (advancing to the next span whose end_col > col, then rejecting it via
        // contains()).
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

    /// Cell-for-cell semantic equality: text, wrap flag, per-char attrs, and
    /// per-column hyperlink / underline-colour answers. Structural
    /// normalization (an empty attrs RLE vs an explicit default run, span-list
    /// coalescing) is not compared, since no reader distinguishes it.
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
            // Sidecars answered per display column over a generous range (wide
            // chars make columns exceed chars; both sides answer None past
            // their spans).
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

    /// Over a corpus mixing every gate dimension (eligible short ASCII, blanks,
    /// soft-wrapped runs, CJK, VS15, hyperlinks, underline colours, stored
    /// WIDE attrs, wrap-changing long lines), the passthrough path is
    /// cell-for-cell identical to the full reference, and the fast path fires.
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
            // Eligible: short plain ASCII (the ~90% shell-history shape).
            corpus.push(styled(&format!("ls -la {i}"), false));
            // Eligible: short ASCII with a real (non-WIDE) attr run.
            corpus.push(Line::with_attrs(
                "AB",
                [styled_attr, CellAttrs::DEFAULT].into_iter().collect(),
            ));
            // Eligible: blank hard-newline row.
            corpus.push(styled("", false));
            // Ineligible: soft-wrapped run (rejoin + re-split).
            corpus.push(styled("ABCDEFGH", false));
            corpus.push(styled("IJKLMNOP", true));
            // Ineligible: CJK (non-ASCII width-2 cells).
            corpus.push(styled("世界世界", false));
            // Ineligible: VS15-narrowed watch with stored WIDE attrs.
            corpus.push(Line::with_attrs(
                "\u{231A}\u{FE0E}Z",
                [wide_attr, wide_attr, CellAttrs::DEFAULT]
                    .into_iter()
                    .collect(),
            ));
            // Ineligible: stored-WIDE ASCII-adjacent shape (write-time wide
            // authority doubles the width, so byte len lies about columns).
            corpus.push(Line::with_attrs(
                "WZ",
                [wide_attr, CellAttrs::DEFAULT].into_iter().collect(),
            ));
            // Ineligible: hyperlink spans (rebuild re-derives span lists).
            corpus.push(Line::with_hyperlinks(
                "ABCD",
                Rle::new(),
                vec![
                    HyperlinkSpan::new(0, 2, Arc::from("u0")),
                    HyperlinkSpan::new(2, 4, Arc::from("u1")),
                ],
            ));
            // Ineligible: underline-colour spans.
            let mut ul = Line::with_hyperlinks("ABCD", Rle::new(), Vec::new());
            ul.set_underline_colors(vec![UnderlineColorSpan::new(0, 2, 0x01_FF_00_00)]);
            corpus.push(ul);
            // Ineligible: wrap-changing long ASCII.
            corpus.push(styled(&format!("L{i}-{}", "x".repeat(50)), false));
        }

        for new_cols in [11u16, 23, 40, 200] {
            let _ = crate::test_counters::take_reflow_passthrough_lines();
            let fast = reflow_scrollback_lines(&corpus, new_cols);
            let fired = crate::test_counters::take_reflow_passthrough_lines();
            let reference = reflow_scrollback_lines_reference(&corpus, new_cols);
            assert_semantically_equal(&fast, &reference);
            // Every "ls -la {i}"/"AB"/blank fits the narrowest width tested
            // (11 cols): the fast path must have fired (reach, side one).
            assert!(
                fired >= 80,
                "passthrough must fire on the eligible majority at {new_cols} \
                 cols (fired {fired})"
            );
        }
    }

    /// Passthrough reach, other side: inputs whose wrap points CAN change — or whose
    /// width the byte length cannot prove — must NEVER take the passthrough.
    #[test]
    fn reflow_passthrough_never_fires_on_wrap_changing_input() {
        let _ = crate::test_counters::take_reflow_passthrough_lines();
        // Longer than the target width: must re-split.
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
        // Non-ASCII single-row lines: widths unprovable by byte length.
        let cjk = vec![styled("世界", false)];
        let _ = reflow_scrollback_lines(&cjk, 40);
        assert_eq!(
            crate::test_counters::take_reflow_passthrough_lines(),
            0,
            "no passthrough for non-ASCII content"
        );
        // A soft-wrapped run of short lines: run length disqualifies it even
        // though each row alone would pass the width gate.
        let run = vec![styled("ABC", false), styled("DEF", true)];
        let _ = reflow_scrollback_lines(&run, 40);
        assert_eq!(
            crate::test_counters::take_reflow_passthrough_lines(),
            0,
            "no passthrough for multi-row logical lines"
        );
    }
}
