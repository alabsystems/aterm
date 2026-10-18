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
    /// Extract the whole off-screen scrollback as logical [`Line`]s, oldest
    /// first (the `get_history_line` order: store, lazy buffer, ring), and
    /// remove it, so the visible-grid reflow runs against an empty history that
    /// the rewrapped output is later prepended to.
    // COST: UNBOUNDED(ring+tiered-history-lines) — materializes the entire
    // off-screen scrollback. Must stay behind the `resize_offloading_scrollback`
    // detach on any main-thread-reachable path (`aterm-census --mainloop`).
    pub(super) fn take_scrollback_lines(&mut self) -> Vec<Line> {
        // `drain_lazy_buffer` pushes lazy lines into the store, so one store
        // sweep covers both.
        self.drain_lazy_buffer();
        let mut lines = Vec::new();
        if let Some(scrollback) = self.storage.scrollback.as_mut() {
            let count = scrollback.line_count();
            lines.reserve(count);
            for i in 0..count {
                match scrollback.get_line(i) {
                    Ok(Some(line)) => lines.push(line.into_owned()),
                    // A decode failure keeps a blank placeholder rather than
                    // truncating older history, so indices stay sane.
                    Ok(None) | Err(_) => lines.push(Line::new()),
                }
            }
            if let Err(error) = scrollback.clear() {
                aterm_log::warn!("scrollback clear during reflow failed: {error}");
            }
        }

        // Then the ring's scrollback rows (the lazy buffer was drained above).
        lines.extend(self.take_ring_scrollback_lines());

        lines
    }

    /// Extract only the ring's scrollback rows as logical [`Line`]s (oldest
    /// first), removing them from the ring; their history extras ride inside
    /// the [`Line`]s and `ring_extras` is cleared. The offloaded width change
    /// uses this to hand the ring history to the detached job.
    // COST: O(ring-scrollback-rows): bounded by the ring capacity (a
    // construction-time constant), not by session history.
    pub(super) fn take_ring_scrollback_lines(&mut self) -> Vec<Line> {
        let ring_scrollback = self.storage.ring_buffer_scrollback();
        if ring_scrollback == 0 {
            return Vec::new();
        }
        // Linearize so logical order == Vec order.
        self.storage.linearize_ring();
        let mut lines = Vec::with_capacity(ring_scrollback);
        // Borrow the stored extras rather than clone them (`ring_extras` and
        // `rows` are disjoint fields); one empty default covers the `None` rows.
        // Declared after `linearize_ring` so the shared borrows never overlap
        // its `&mut`.
        let no_extras = super::ScrolledRowExtras::default();
        for i in 0..ring_scrollback {
            let extras = self.storage.ring_history_extras(i).unwrap_or(&no_extras);
            let row = &self.storage.rows[i];
            // The successor is the next ring row or the first visible row.
            let len = history_row_len(row, self.storage.rows.get(i + 1));
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

    /// Push rewrapped scrollback [`Line`]s back into history as its oldest part,
    /// ahead of any overflow the visible-grid reflow produced.
    ///
    /// With a store that retains lines (`GridStorage::store_retains_lines`) they
    /// go straight into it (`push_line` honors its line limit); otherwise they
    /// become ring scrollback rows up to `max_scrollback`, older lines evicted.
    /// A store whose share of the total is zero takes the ring route: such a
    /// grid is a ring-only grid of cap = total, so the newest `max_scrollback`
    /// lines of [old history | the reflow's overflow] become ring rows. Decided
    /// here rather than in `finalize_reflow` because the overflow is the newest
    /// history, the lines the cap keeps.
    ///
    /// `staged_before` is the lazy buffer's length before the visible reflow
    /// staged its overflow: everything past it is this resize's overflow, and
    /// anything before it is older output a detach window staged.
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
            // `take_scrollback_lines` cleared the store and the lazy buffer holds
            // only the visible reflow's overflow, so pushing the older history
            // directly, then draining, keeps [old scrollback | overflow] order.
            if let Some(scrollback) = self.storage.scrollback.as_mut() {
                for line in lines {
                    if let Err(error) = scrollback.push_line(line) {
                        aterm_log::warn!("scrollback push_line during reflow failed: {error}");
                    }
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
            // Ring-only grid, a store that keeps nothing, or a detach window: with
            // no store to keep it, the visible reflow's staged overflow would be
            // discarded by the next `drain_lazy_buffer`, and read in the wrong
            // order meanwhile (it is newer than `lines`). Absorb only this
            // resize's own overflow as the newest tail: a window's older staged
            // output stays staged ahead of both for re-attach, so history reads
            // [staged | `lines` | overflow].
            let overflow = self.storage.lazy_buffer.len().saturating_sub(staged_before);
            if overflow > 0 {
                lines.extend(self.storage.lazy_buffer.drain_back(overflow));
            }
            if !lines.is_empty() {
                self.prepend_ring_scrollback_lines(lines, new_cols);
            }
        }
    }

    /// Convert reflowed scrollback [`Line`]s into ring scrollback rows and
    /// prepend them ahead of the visible window, honoring `max_scrollback`.
    pub(super) fn prepend_ring_scrollback_lines(&mut self, lines: Vec<Line>, new_cols: u16) {
        // Only the newest `max_scrollback` lines fit the ring.
        let cap = self.storage.max_scrollback;
        let skip = lines.len().saturating_sub(cap);
        // During a detach window the store cannot take the lines past the cap,
        // so stage them to the lazy buffer (flushed on re-attach) behind its
        // older content; the newer `kept` lines become ring scrollback. Only
        // where a store will keep them (`stages_evicted_rows`): a staged line
        // no store keeps would outlive newer ring rows that later scrolls drop.
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

        // Linearize so we can splice scrollback rows at the front (index 0).
        self.storage.linearize_ring();

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
        // every absolute row number shifts: bump `history_renumber_epoch` so
        // absolute-row-keyed caches (search index, viewport row cache) rebuild.
        // A spurious rebuild is harmless; a missed one is silently wrong.
        self.storage.history_renumber_epoch = self.storage.history_renumber_epoch.saturating_add(1);
        // A bulk re-entry into the ring (and, detached, the staged rows).
        self.enforce_history_link_budget();
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
        let extras = super::scroll_fill::fill_row_into(&mut row, line, new_cols);
        (row, extras)
    }
}

/// How many cells of a history row (one leaving the grid as a [`Line`]) are
/// its logical line's content.
///
/// A row whose logical line continues (its successor is a wrap continuation)
/// was filled to its last column by autowrap, so its trailing blanks are
/// content: it is read at its full width, or a width sweep erodes one space per
/// chunk boundary. Only a recorded early-wrap hole is left out
/// ([`Row::continued_len`](crate::Row::continued_len)): a successor that opens
/// with a wide glyph says nothing about the last cell, which may hold a space
/// or a glyph the program wrote there. Any other row is read to its length.
pub(super) fn history_row_len(row: &crate::Row, successor: Option<&crate::Row>) -> u16 {
    if row.line_size() == super::LineSize::SingleWidth
        && successor.is_some_and(crate::Row::is_wrapped)
    {
        row.continued_len()
    } else {
        row.len()
    }
}

/// Maximum display columns a logical line may span before accumulation stops,
/// guarding against pathological inputs.
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

/// Rewrap scrollback [`Line`]s to `new_cols`, keeping hard newlines and
/// content: each soft-wrapped run (lines after the first carry the wrapped
/// flag) is joined, then re-split by display width. O(total cells) worst case;
/// single-row lines whose wrap points cannot change pass through as clones
/// (`rewrap_passthrough_eligible`).
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
        // at `new_cols` is cloned, skipping decomposition and rebuild (gate:
        // `rewrap_passthrough_eligible`; the `reflow_passthrough_*` tests pin it
        // to the full path).
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

/// The newest `rows` rows of [`reflow_scrollback_lines`]`(lines, new_cols)`,
/// rewrapping only the newest logical lines that fill them.
///
/// The rewrap goes logical line by logical line, so a suffix of `lines` that
/// opens on a head rewraps to exactly the tail of the whole rewrap; older lines
/// are never rewrapped. Used where a seat holds `rows` and drops the rest (the
/// ring's room when the re-attached store keeps nothing).
// COST: O(rows × new_cols + one logical line): every logical line rewrapped
// but the oldest one visited yields only kept rows, at most `new_cols` cells
// each.
#[must_use]
pub(super) fn reflow_newest_rows(lines: &[Line], new_cols: u16, rows: usize) -> Vec<Line> {
    let mut runs: Vec<Vec<Line>> = Vec::new();
    let mut produced = 0;
    let mut end = lines.len();
    while end > 0 && produced < rows {
        // The logical line ending at `end` opens at the nearest line that is
        // not a soft-wrap continuation (an orphaned continuation at the very
        // front opens one too, as in `reflow_scrollback_lines`).
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

/// Wrap-invariance gate for the clone-through fast path. True only for:
///
/// * a single-row logical line (checked by the caller; a leading orphaned
///   continuation is fine, since `cloned_for_rewrap` resets the flags as
///   `build_line` would);
/// * printable ASCII (0x20..=0x7E), so byte length is display width;
/// * that length fitting `new_cols`;
/// * no stored WIDE flag (`stored_unit_is_wide` can widen a cell `char_width`
///   calls narrow; `rewrap_preserves_stored_ambiguous_width_with_narrow_control`);
/// * no hyperlink or SGR 58 underline-colour spans (the rebuild may re-coalesce
///   them);
/// * no inline-image spans (the rebuild clips them at the new right margin, as
///   a live placement does; a clone would not).
///
/// Other attributes need no gate: the rebuild copies per-cell attrs verbatim.
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
/// the live grid clips tail tiles at a narrower right margin. A multi-line run
/// drops its images: their columns are re-bucketed, so no segment describes
/// the footprint. `place_image` line-feeds between footprint rows, so this
/// arises only when an image is stamped onto a row whose text then soft-wraps.
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
    // `unit_char_start` and `col` are monotone, so advancing cursors read the
    // RLE attrs and the sorted, disjoint link and underline-colour spans in
    // O(runs + spans) total, not per cell: this walk covers all retained
    // history on every width change.
    let mut attr_cursor = line.attr_cursor();
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
        // Effective width, replaying the live VS15 transitions and taking a
        // VS16 widening from the stored bit (`stored_unit_is_wide`), so
        // reflowed rows keep the live columns (`❤️` two, `⌚︎` one) and `col`
        // stays the column the line's link and underline-colour spans name. An
        // edge-pinned VS16 unit stays narrow, as the screen's own rewrap keeps
        // it; re-widening it would shift every later unit's span lookup.
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

    /// A rewrap joins each soft-wrapped run into its logical line and splits
    /// it at the new width (first row unwrapped, the rest continuations), never
    /// across a hard newline or through a wide char, and keeps a blank line.
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

    /// Hyperlink and underline-colour spans follow their cells across a split,
    /// and a cell in the gap between two spans carries neither.
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

    /// Passthrough reach, other side: inputs whose wrap points can change, or
    /// whose width the byte length cannot prove, never take the passthrough.
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

    /// `reflow_newest_rows` is the tail of the whole rewrap, for every row
    /// count up to past the whole output: over runs that narrow and widen, an
    /// orphaned continuation at the front, wide cells and blank rows. And it
    /// rewraps only the newest logical lines that fill the rows asked for:
    /// over single-row lines that pass through, one row asked rewraps one.
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
