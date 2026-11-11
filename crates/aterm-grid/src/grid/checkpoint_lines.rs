// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Line-level grid checkpoint projection and visible-row restoration.

use aterm_scrollback::{ImageSpan, Line, Scrollback, ScrollbackStorage};

use super::Grid;

/// Inline-image spans carried beside a line-serialized [`Grid`] checkpoint: sparse visible rows (`0..rows`) and reverse-indexed uncompressed history rows (`0` = newest scrollback line).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GridImageCarry {
    /// Non-empty visible-row span lists `(row, spans)` sorted ascending by `row`.
    pub visible: Vec<(u16, Vec<ImageSpan>)>,
    /// Non-empty history-row span lists `(rev_idx, spans)` sorted ascending by `rev_idx` (`0` = newest).
    pub history_rev: Vec<(u32, Vec<ImageSpan>)>,
}

impl GridImageCarry {
    /// Whether this carry holds neither visible nor history image spans.
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.visible.is_empty() && self.history_rev.is_empty()
    }

    /// Copy with `history_rev` truncated to `rev_idx < max_history`.
    #[must_use]
    pub fn bounded(&self, max_history: usize) -> Self {
        Self {
            visible: self.visible.clone(),
            history_rev: self
                .history_rev
                .iter()
                .filter(|(rev, _)| (*rev as usize) < max_history)
                .cloned()
                .collect(),
        }
    }
}

impl Grid {
    /// Projects at most `max_history` recent scrollback lines followed by the visible rows.
    #[must_use]
    pub fn checkpoint_lines_bounded(&self, max_history: usize) -> Vec<Line> {
        let scrollback_count = self.scrollback_lines();
        let keep = scrollback_count.min(max_history);
        let first = scrollback_count - keep;
        let mut lines = Vec::with_capacity(keep + self.rows() as usize);
        lines.extend(
            self.history_lines_from(first)
                .take(keep)
                .map(|line| line.map_or_else(Line::new, std::borrow::Cow::into_owned)),
        );
        lines.extend(self.checkpoint_visible_lines());
        lines
    }

    /// Projects recent history and visible rows, borrowing materialized history
    /// instead of cloning its text and attributes. Cold history and live rows
    /// that need conversion remain owned by the returned view.
    #[must_use]
    pub fn checkpoint_line_views_bounded(
        &self,
        max_history: usize,
    ) -> Vec<std::borrow::Cow<'_, Line>> {
        let scrollback_count = self.scrollback_lines();
        let keep = scrollback_count.min(max_history);
        let first = scrollback_count - keep;
        let mut lines = Vec::with_capacity(keep + self.rows() as usize);
        lines.extend(
            self.history_lines_from(first)
                .take(keep)
                .map(|line| line.unwrap_or_else(|| std::borrow::Cow::Owned(Line::new()))),
        );
        lines.extend(
            self.checkpoint_visible_lines()
                .into_iter()
                .map(std::borrow::Cow::Owned),
        );
        lines
    }

    /// Projects only the live visible rows into `Vec<Line>`.
    #[must_use]
    pub fn checkpoint_visible_lines(&self) -> Vec<Line> {
        let rows = self.rows();
        let mut lines = Vec::with_capacity(rows as usize);
        let extras = self.extras();
        for r in 0..rows {
            let Some(row) = self.row_at_screen(r) else {
                lines.push(Line::new());
                continue;
            };
            let row_extras = Self::extract_row_extras(row, extras, r);
            let len = super::scrollback_reflow::history_row_len(row, self.row_at_screen(r + 1));
            lines.push(Self::row_to_line_with_owned_extras_at_len(
                row, row_extras, len,
            ));
        }

        lines
    }

    /// Restores visible rows `0..rows()` of this grid from `visible`.
    pub fn restore_visible_from_lines(&mut self, visible: &[Line]) {
        let rows = self.rows();
        let cols = self.cols();
        let Some(first) = (0..rows).find(|&r| self.row(r).is_some()) else {
            return;
        };
        let refill = self.storage.extras.prepare_rows_for_restore(first..rows);
        let mut owned_text = false;
        for r in first..rows {
            match visible.get(r as usize) {
                Some(line) => owned_text |= self.fill_row_from_line_cleared(r, line, cols),
                None => {
                    if let Some(row) = self.row_mut(r) {
                        row.clear();
                    }
                }
            }
        }
        self.storage.extras.finish_rows_restore(refill, owned_text);
        self.storage.extras.maybe_shrink();
    }

    /// Clones this grid's history, live frame, and cursor state for mid-recording replay.
    #[must_use]
    pub fn replay_fork(&self) -> Option<Grid> {
        let storage = &self.storage;
        if storage.scrollback_detached_for_reflow
            || storage.resize_undo.is_some()
            || storage.compress_offload_active
            || storage.staged_width_stale
        {
            return None;
        }
        let (rows, cols) = (self.rows(), self.cols());
        let lines = self.checkpoint_lines_bounded(usize::MAX);
        let (history, visible) = lines.split_at(lines.len().saturating_sub(usize::from(rows)));
        let mut fork = match storage.scrollback.as_ref() {
            None => {
                let mut fork = Grid::with_scrollback(rows, cols, storage.max_scrollback);
                fork.scroll_in(history);
                fork
            }
            Some(store) => self.tiered_fork(store, history)?,
        };
        fork.restore_visible_from_lines(visible);
        let from = &storage.cursor_state;
        let to = &mut fork.storage.cursor_state;
        to.cursor = from.cursor;
        to.saved_cursor = from.saved_cursor;
        to.scroll_region = from.scroll_region;
        to.horizontal_margins = from.horizontal_margins;
        to.tab_stops.clone_from(&from.tab_stops);
        to.tab_defaults_suppressed = from.tab_defaults_suppressed;
        to.pending_wrap = from.pending_wrap;
        to.follows_glyph = from.follows_glyph;
        to.cursor_template = from.cursor_template;
        to.cursor_template_bg_rgb = from.cursor_template_bg_rgb;
        fork.storage.has_horizontal_margins = storage.has_horizontal_margins;
        fork.storage.any_double_width = storage.any_double_width;
        fork.storage.resize_law = storage.resize_law;
        fork.continue_absolute_numbering(self.absolute_row_counter());
        fork.storage.handback = storage.handback;
        fork.storage.replay_carried_history.store(
            storage
                .replay_carried_history
                .load(std::sync::atomic::Ordering::Relaxed),
            std::sync::atomic::Ordering::Relaxed,
        );
        fork.storage.deep_history_pullback_ordinal = storage.deep_history_pullback_ordinal;
        Some(fork)
    }

    /// A tiered grid laid out as this one is: its store's lines in a store with the same limits, its staged lines staged, and its ring's lines in the ring.
    fn tiered_fork(&self, store: &ScrollbackStorage, history: &[Line]) -> Option<Grid> {
        let stored = store.line_count();
        let staged = stored.checked_add(self.storage.lazy_buffer_lines())?;
        if staged.checked_add(self.storage.ring_buffer_scrollback()) != Some(history.len()) {
            return None;
        }
        let mut copy = Scrollback::with_defaults();
        copy.set_line_limit(store.line_limit());
        copy.set_memory_budget(store.memory_budget()).ok()?;
        for line in &history[..stored] {
            copy.push_line(line.clone());
        }
        let (rows, cols) = (self.rows(), self.cols());
        let mut fork = Grid::with_tiered_scrollback(rows, cols, self.storage.max_scrollback, copy);
        for line in &history[stored..staged] {
            let (row, extras) = fork.build_scrollback_row(line, cols);
            fork.storage.lazy_buffer.push_row(&row, extras);
            // SAFETY: build_scrollback_row allocated this temporary row in fork.storage.pages, and the lazy buffer owns its copied cell data.
            unsafe { row.recycle(&mut fork.storage.pages) };
        }
        fork.scroll_in(&history[staged..]);
        Some(fork)
    }

    /// Scrolls `lines` (oldest first) through the top row into history.
    pub fn scroll_in_history(&mut self, lines: &[Line]) {
        self.scroll_in(lines);
    }

    /// Captures inline-image spans across visible rows and at most `max_history` newest scrollback rows (`0` = newest).
    #[must_use]
    pub fn capture_image_carry(&self, max_history: usize) -> GridImageCarry {
        let has_visible = self.extras().any_image();
        let has_history = max_history > 0 && self.history_any_image(&|_| true);
        if !has_visible && !has_history {
            return GridImageCarry::default();
        }
        let mut visible = Vec::new();
        if has_visible {
            let rows = self.rows();
            let extras = self.extras();
            let mut scratch = super::ScrolledRowExtras::default();
            for r in 0..rows {
                let Some(row) = self.row_at_screen(r) else {
                    continue;
                };
                scratch.clear();
                Self::extract_image_spans_into(&mut scratch, row, extras, r);
                if !scratch.images.is_empty() {
                    visible.push((r, std::mem::take(&mut scratch.images)));
                }
            }
        }
        let mut history_rev = Vec::new();
        if has_history {
            let total = self.scrollback_lines();
            let cap = total.min(max_history).min(u32::MAX as usize);
            let ring = self.storage.ring_buffer_scrollback();
            let lazy = self.storage.lazy_buffer_lines();
            let tiered = self
                .storage
                .scrollback
                .as_ref()
                .map_or(0, ScrollbackStorage::line_count);
            for rev in 0..cap {
                let spans = if rev < ring {
                    let ring_idx = ring - 1 - rev;
                    self.storage
                        .ring_history_extras(ring_idx)
                        .map(|e| e.images.as_slice())
                        .filter(|s| !s.is_empty())
                } else {
                    let after_ring = rev - ring;
                    if after_ring < lazy {
                        let lazy_idx = lazy - 1 - after_ring;
                        self.storage.lazy_buffer.line_images(lazy_idx)
                    } else {
                        let store_rev = after_ring - lazy;
                        if store_rev < tiered {
                            let store_idx = tiered - 1 - store_rev;
                            self.storage
                                .scrollback
                                .as_ref()
                                .and_then(|sb| sb.line_images(store_idx))
                                .filter(|s| !s.is_empty())
                        } else {
                            None
                        }
                    }
                };
                if let Some(spans) = spans
                    && let Ok(rev_u32) = u32::try_from(rev)
                {
                    history_rev.push((rev_u32, spans.to_vec()));
                }
            }
        }
        GridImageCarry {
            visible,
            history_rev,
        }
    }

    /// Installs inline-image spans captured by [`Self::capture_image_carry`] onto visible and uncompressed history rows.
    pub fn apply_image_carry(&mut self, carry: &GridImageCarry) {
        if self.extras().any_image() {
            let rows = self.rows();
            let cols = self.cols();
            let coords: Vec<(u16, u16)> = self
                .extras()
                .iter()
                .filter(|(at, extra)| at.row < rows && at.col < cols && !extra.images().is_empty())
                .map(|(at, _)| (at.row, at.col))
                .collect();
            for (r, c) in coords {
                self.cell_extra_mut(r, c).retain_images(|_| false);
            }
        }
        if self.history_any_image(&|_| true) {
            self.retain_history_images(&mut |_| false);
        }
        if carry.is_empty() {
            return;
        }
        let rows = self.rows();
        let cols = self.cols();
        for (row, spans) in &carry.visible {
            if *row < rows && !spans.is_empty() {
                self.stamp_row_image_spans(*row, spans, cols);
            }
        }
        let ring = self.storage.ring_buffer_scrollback();
        let lazy = self.storage.lazy_buffer_lines();
        let mut history_changed = false;
        for (rev_u32, spans) in &carry.history_rev {
            if spans.is_empty() {
                continue;
            }
            let Ok(rev) = usize::try_from(*rev_u32) else {
                continue;
            };
            if rev < ring {
                let ring_idx = ring - 1 - rev;
                history_changed |= self.storage.ring_extras.set_images(ring_idx, spans.clone());
            } else {
                let after_ring = rev - ring;
                if after_ring < lazy {
                    let lazy_idx = lazy - 1 - after_ring;
                    history_changed |= self
                        .storage
                        .lazy_buffer
                        .set_line_images(lazy_idx, spans.clone());
                } else {
                    let store_rev = after_ring - lazy;
                    if let Some(store) = self.storage.scrollback.as_mut() {
                        history_changed |= store.set_hot_line_images(store_rev, spans.clone());
                    }
                }
            }
        }
        if history_changed || !carry.visible.is_empty() {
            self.storage.mark_content_full();
        }
    }

    /// Scrolls `lines` (oldest first) through the top row into history.
    fn scroll_in(&mut self, lines: &[Line]) {
        let cols = self.cols();
        for line in lines {
            self.fill_row_from_line(0, line, cols);
            self.scroll_up(1);
        }
    }
}

#[cfg(test)]
#[path = "checkpoint_lines_tests.rs"]
mod tests;
