// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grid reflow: rewrap lines when terminal column count changes.
//!
//! O(rows × cols) complexity, verified by `reflow_linear_time*` tests (#672).

#[path = "reflow_map.rs"]
mod reflow_map;

use self::reflow_map::{
    ExtrasCopyCtx, ExtrasSource, chunk_cells_to_rows, copy_cells_to_row, source_coords_for_row,
};
use super::row_u16;
use super::{CellCoord, CellExtras, Grid};
use crate::Damage;
use crate::LineSize;
use crate::PageStore;
use crate::Row;
use crate::{MAX_GRID_COLS, MAX_GRID_ROWS};

/// Selects whether resize should reflow wrapped content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReflowMode {
    Enabled,
    Disabled,
}

impl From<bool> for ReflowMode {
    fn from(reflow: bool) -> Self {
        if reflow {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }
}

struct ReflowResult {
    rows: Vec<Row>,
    pages: PageStore,
    extras: CellExtras,
    cursor_row: usize,
    cursor_col: u16,
}

/// Resize a DECDWL/DECDHL row in place (truncate or pad) without reflow.
///
/// Double-width and double-height lines are logically half-width: each
/// character occupies two physical columns in the renderer. Reflowing them
/// would split the logical line across multiple rows, corrupting the
/// display. Instead we copy cells up to `min(content_len, new_cols)` and
/// pad the remainder (#7524).
#[allow(clippy::too_many_arguments)]
fn resize_double_width_row_in_place(
    row: &Row,
    row_idx: usize,
    new_cols: u16,
    new_pages: &mut PageStore,
    new_rows: &mut Vec<Row>,
    cursor_row: usize,
    cursor_col: u16,
    cursor: &mut (usize, u16),
    old_extras: Option<&CellExtras>,
    new_extras: &mut CellExtras,
) {
    let content_len = row.len() as usize;
    // SAFETY: `new_row` is appended to `new_rows` and returned alongside
    // `new_pages` in the same reflow result.
    let mut new_row = unsafe { Row::new(new_cols, new_pages) };
    // Set line_size BEFORE copying cells. set_line_size(DoubleWidth) clears
    // cells from cols/2 onward, so it must precede copy_cells_to_row which
    // will overwrite those cleared positions with the actual content.
    new_row.set_line_size(row.line_size());
    if row.is_wrapped() {
        new_row.set_wrapped(true);
    }
    if content_len > 0 {
        let cells = &row.as_slice()[..content_len];
        let copy_len = content_len.min(new_cols as usize);
        let dest_row = row_u16(new_rows.len());
        let mut extras_ctx = ExtrasCopyCtx {
            // Single-row copy → compute coords (no throwaway per-row Vec).
            source: if old_extras.is_some() {
                ExtrasSource::Row(row_u16(row_idx))
            } else {
                ExtrasSource::None
            },
            old_extras,
            new_extras,
        };
        copy_cells_to_row(
            &mut new_row,
            cells,
            0,
            copy_len,
            new_cols,
            dest_row,
            &mut extras_ctx,
        );
    }
    if cursor_row == row_idx {
        *cursor = (new_rows.len(), cursor_col.min(new_cols.saturating_sub(1)));
    }
    new_rows.push(new_row);
}

impl Grid {
    /// Resize the grid, reflowing content if column count changed.
    pub fn resize(&mut self, new_rows: u16, new_cols: u16) {
        self.resize_with_reflow_mode(new_rows, new_cols, ReflowMode::Enabled);
    }

    /// Resize without reflow (for alt-screen grids that redraw after `SIGWINCH`).
    pub fn resize_no_reflow(&mut self, new_rows: u16, new_cols: u16) {
        self.resize_with_reflow_mode(new_rows, new_cols, ReflowMode::Disabled);
    }

    /// Resize the grid with explicit reflow mode.
    // COST: UNBOUNDED(scrollback-width-reflow) — the width branch calls
    // `take_scrollback_lines` + `reflow_scrollback_lines` SYNCHRONOUSLY. See
    // `xtask gate mainloop` (MAIN-LOOP COMPLETENESS CENSUS): a main-thread reach
    // to this under the `term` lock is the L0 whole-Mac freeze. `Grid::resize` /
    // `Terminal::resize` forward here; the offloaded path detaches history first.
    pub fn resize_with_reflow_mode(
        &mut self,
        new_rows: u16,
        new_cols: u16,
        reflow_mode: ReflowMode,
    ) {
        let reflow = matches!(reflow_mode, ReflowMode::Enabled);
        // Ingress clamp (§5.8): bound the allocation a hostile resize can request.
        let new_rows = new_rows.clamp(1, MAX_GRID_ROWS);
        let new_cols = new_cols.clamp(1, MAX_GRID_COLS);
        let old_cols = self.storage.cols;

        // Snap to the live view for the reflow/resize computation below — it operates
        // on the live grid + scrollback split and assumes display_offset 0 (#2184) —
        // but REMEMBER the user's scrollback position first so it can be restored at
        // the end. A window resize / font zoom must not yank a reader who is scrolled
        // back in history down to the live bottom.
        let prev_offset = self.storage.display_offset;
        self.storage.display_offset = 0;

        // On a width change with reflow, lift the entire off-screen scrollback
        // out and rewrap it to the new width BEFORE any visible-grid mutation,
        // so history survives the resize (#7906). Reads ring_extras, so it must
        // precede the ring_extras.clear() below. Restored after adjust_row_count.
        let reflowed_scrollback = if new_cols != old_cols && reflow {
            let old = self.take_scrollback_lines();
            // Bounded-cost obligation: every line counted here was rewrapped
            // SYNCHRONOUSLY on the caller's thread (under its lock). This must be
            // bounded by the viewport, not by session history — a deep-history
            // resize that lands here is the L0 whole-Mac freeze. The offloaded
            // path (`resize_offloading_scrollback`) drives this count to zero.
            #[cfg(any(test, feature = "testing"))]
            super::count_scrollback_reflow_sync_lines(old.len());
            let lines = super::scrollback_reflow::reflow_scrollback_lines(&old, new_cols);
            Some(lines)
        } else {
            None
        };

        // Ring extras and lazy buffer are invalidated by a ring-buffer rebuild
        // (#4149, #4215) — but a rows-only resize of a RING-ONLY grid does not
        // rebuild the ring (history is reclassified in place, see
        // adjust_row_count_ring_only), so its history extras must survive.
        let rows_only_ring_only = new_cols == old_cols
            && self.storage.scrollback.is_none()
            && !self.storage.scrollback_detached_for_reflow;
        if !rows_only_ring_only {
            self.storage.ring_extras.clear();
            // Drain lazy buffer: deferred lines reference pre-reflow cell data.
            self.drain_lazy_buffer();
        }

        // That drain just recycled up to a poolful of OLD-WIDTH cell bodies.
        // Their capacities are sized for `old_cols`, so keeping them across a
        // width change is either dead memory (width shrank) or a guaranteed
        // realloc on first refill (width grew). Contents cannot go stale — the
        // body is cleared and refilled from the row slice — so this is purely a
        // footprint/realloc guard, and the pool refills within one drain batch.
        if new_cols != old_cols {
            self.storage.lazy_buffer.clear_pool();
        }

        let cursor_row = self.storage.cursor.row as usize;
        let cursor_col = self.storage.cursor.col;

        if new_cols != old_cols && reflow {
            self.reflow_columns(new_rows, new_cols, cursor_row, cursor_col);
        } else if new_cols != old_cols {
            // No reflow - just resize each row
            let mut new_pages = PageStore::new();
            for row in &mut self.storage.rows {
                // SAFETY: `new_pages` stays alive until it replaces `self.storage.pages`
                // at the end of this branch, so every resized row keeps a live
                // backing store for at least as long as the row remains in `self`.
                unsafe { row.resize(new_cols, &mut new_pages) };
            }
            self.storage.pages = new_pages;
            // Discard extras beyond the new column count (#7280).
            // Without this, hyperlinks/RGB colors in truncated columns
            // remain as orphaned entries until the next full grid clear.
            if new_cols < old_cols {
                self.storage.extras.retain_cols_below(new_cols);
            }
        }

        // The column-reflow path rebuilds a fresh CellExtras and migrates each
        // cell's ring-stored non-BMP codepoint into it (the #7447 fallback). The
        // rows-only and no-reflow paths keep the existing extras and instead drop
        // BOTH rings via `invalidate_rings` below — which would strand every
        // on-screen non-BMP (emoji/CJK-SMP) cell as U+FFFD, since the ring holds
        // its codepoint. Harvest those codepoints into the persistent HashMap
        // FIRST (at pre-`adjust_row_count` positions, so they ride the same row
        // shift as combining marks) whenever the column reflow did not run.
        if !(new_cols != old_cols && reflow) {
            self.migrate_complex_ring_to_extras();
        }

        let revealed = self.adjust_row_count(new_rows, new_cols);
        // Discard CellExtras entries for rows that were removed during
        // adjust_row_count. Without this, orphaned HashMap entries for
        // deleted rows leak memory until the next full grid clear. (#7409)
        self.storage.extras.retain_rows_below(new_rows);
        // Invalidate ring buffers — their stride/visible_rows are stale after
        // any dimension change. They will be lazily re-created on next write
        // with the correct dimensions. The reflow-enabled path already creates
        // a fresh CellExtras, but no-reflow and row-only resize paths do not.
        self.storage.extras.invalidate_rings();
        self.storage.resize_viewport_state(new_rows, new_cols);
        // A rows-grow that revealed history re-labelled the newest ring lines
        // as the TOP of the viewport — every pre-resize viewport row (the
        // cursor's included) now sits `revealed` rows further down. Follow it,
        // or the cursor points `revealed` rows ABOVE its content and an inline
        // TUI's post-SIGWINCH repaint (and its CPR answers) anchor wrong,
        // painting into the revealed band. The column-reflow path owns its own
        // cursor tracking (`reflow_columns`), so only the paths that did not
        // reflow compensate here; `resize_viewport_state` already clamped, so
        // the shift re-clamps against the SAME bound.
        if revealed > 0 && !(new_cols != old_cols && reflow) {
            // Publish the same shift for the SELECTION, which is compensated by
            // `Terminal::finalize_resize` rather than here — it lives on the
            // Terminal, not the Grid. Same guard as the cursor's for the same
            // reason: the column-reflow path owns its own tracking.
            self.storage.last_resize_row_shift = row_u16(revealed);
            let bound = new_rows.saturating_sub(1);
            let row = self
                .storage
                .cursor
                .row
                .saturating_add(row_u16(revealed))
                .min(bound);
            let col = self.storage.clamp_col_for_row(row, self.storage.cursor.col);
            self.storage.set_cursor_position(row, col);
            if self.storage.saved_cursor.valid {
                let saved = self
                    .storage
                    .saved_cursor
                    .cursor
                    .row
                    .saturating_add(row_u16(revealed))
                    .min(bound);
                self.storage.saved_cursor.cursor.row = saved;
                self.storage.saved_cursor.cursor.col = self
                    .storage
                    .clamp_col_for_row(saved, self.storage.saved_cursor.cursor.col);
            }
        }
        // Restore the rewrapped history as the front (oldest) of the scrollback,
        // after the visible grid is finalized so adjust_row_count cannot trim it
        // and the new dimensions are in place (#7906).
        if let Some(lines) = reflowed_scrollback {
            self.restore_reflowed_scrollback(lines, new_cols);
        }
        // Restore the pre-resize scrollback position, clamped to the new history
        // length. After a width reflow the wrapped-line count changes, so an exact
        // line anchor isn't possible — but staying in history (clamped) is far better
        // than snapping a scrolled-back reader to the live bottom on every resize/zoom.
        self.storage.display_offset = prev_offset.min(self.storage.scrollback_lines());
        self.storage.pages.shrink_to_fit();
        self.storage.damage = Damage::Full;
        // Reflow/resize rewraps line content, so it is a CONTENT change even
        // though it assigns `Damage::Full` directly rather than via a
        // `mark_content_*` wrapper — bump the content generation so a cached
        // search index (and cross-session change poll) invalidates correctly.
        self.storage.content_gen += 1;
    }

    /// Copy every VISIBLE complex cell's ring-stored codepoint into the persistent
    /// HashMap extras, so a resize that does NOT rebuild extras via column reflow
    /// (rows-only / no-reflow) does not lose on-screen non-BMP cells when
    /// [`invalidate_rings`](crate::extra_collection::CellExtras::invalidate_rings)
    /// clears the ring. Mirrors the column reflow's ring fallback (#7447). Gated on
    /// the cell's COMPLEX flag so stale ring slots left by overwritten cells are not
    /// resurrected, and skips cells already HashMap-backed (ZWJ/skin-tone clusters).
    fn migrate_complex_ring_to_extras(&mut self) {
        // Only VISIBLE rows can match: `GridStorage::row` returns `None` for any
        // index >= `visible_rows`, so the ring-scrollback tail of `rows` was pure
        // dead work — up to `max_scrollback` (10_000 in the GUI) x cols no-op
        // probes per rows-only resize, on the main thread under the `term` lock.
        // `.min(rows.len())` keeps the bound never LARGER than today's, so a
        // degenerate `visible_rows > rows.len()` state cannot start aliasing
        // through `row_index`'s `% rows.len()` and migrate a coord twice.
        let rows = usize::from(self.storage.visible_rows).min(self.storage.rows.len());
        let cols = self.storage.cols;
        for r in 0..rows {
            let row = row_u16(r);
            for col in 0..cols {
                let is_complex = self
                    .row(row)
                    .and_then(|rw| rw.get(col))
                    .is_some_and(|cell| cell.is_complex());
                if !is_complex {
                    continue;
                }
                // Already HashMap-backed (multi-char cluster) — nothing to migrate.
                if self.storage.extras.complex_char_arc_for(row, col).is_some() {
                    continue;
                }
                if let Some(ch) = self.storage.extras.complex_codepoint_for(row, col) {
                    let mut buf = [0u8; 4];
                    self.set_cell_complex_char(row, col, ch.encode_utf8(&mut buf));
                }
            }
        }
    }

    /// Trim (front then back) or grow the row buffer to match `target_rows`.
    ///
    /// When the visible row count decreases, excess rows at the front of the
    /// ring buffer (scrollback rows) are pushed to the lazy buffer as
    /// `DeferredLine`s before being drained, preserving scrollback content
    /// across height decreases (#7473).
    /// Returns how many history lines the viewport REVEALED on a rows-grow:
    /// ring-held lines directly above the old viewport that the caller's
    /// `visible_rows` update re-labels as visible content at the TOP of the
    /// screen. The caller owes the cursor a downward shift by exactly this
    /// count — the cursor's content moved that many rows down the viewport.
    fn adjust_row_count(&mut self, target_rows: u16, new_cols: u16) -> usize {
        let target = target_rows as usize;
        let old_visible = usize::from(self.storage.visible_rows);

        // A ROWS-ONLY resize of a RING-ONLY grid (no tiered store, not
        // mid-detach — the wasm engines' shape) must not shed ring history to
        // fit the viewport: the ring IS retention there. Reclassify viewport
        // rows against ring history in place instead of the store migration
        // below. Identity law: same logical buffer (history sequence,
        // viewport, absolute numbering) as the tiered path produces for the
        // same resize. Width changes keep the pre-existing machinery (their
        // ring history rides take_scrollback_lines / restore, and this method
        // then runs against reflow_columns' freshly rebuilt rows).
        let has_scrollback =
            self.storage.scrollback.is_some() || self.storage.scrollback_detached_for_reflow;
        if !has_scrollback && new_cols == self.storage.cols {
            return self.adjust_row_count_ring_only(target, new_cols);
        }

        if self.storage.rows.len() > target {
            // Linearize ring buffer so pop/drain operate on logical order.
            let ring_head = self.storage.ring_head;
            if ring_head != 0 {
                self.storage.rows.rotate_left(ring_head);
                self.storage.ring_head = 0;
            }
            let excess = self.storage.rows.len() - target;
            let scrollback = self
                .storage
                .total_lines
                .saturating_sub(self.storage.visible_rows as usize);
            let from_front = excess.min(scrollback);
            let from_back = excess - from_front;
            if from_front > 0 {
                // Push front rows to lazy scrollback before draining (#7473).
                // Only when tiered scrollback is attached, matching the
                // scroll.rs pattern. Without tiered scrollback, deferred
                // lines would sit in the lazy buffer indefinitely since
                // drain_lazy_buffer discards them when no scrollback exists.
                // Also stage while the store is detached for an off-thread
                // reflow: a height shrink racing the reflow window must not
                // drop ring scrollback (window output) — the lazy buffer is
                // flushed on re-attach, matching scroll.rs (audit bug B).
                let has_scrollback = self.storage.scrollback.is_some()
                    || self.storage.scrollback_detached_for_reflow;
                if has_scrollback {
                    let drained_rows: Vec<Row> = self.storage.rows.drain(..from_front).collect();
                    for row in &drained_rows {
                        // These are scrollback rows whose CellExtras were already
                        // extracted during normal scroll_up. Use u16::MAX as
                        // row_idx so HashMap-keyed lookups (hyperlinks, combining
                        // marks) don't misattribute visible-row extras to these
                        // scrollback rows (#7513). Ring-buffer lookups use the
                        // cell's internal index, unaffected by row_idx.
                        let extracted = Self::extract_row_extras(
                            row,
                            &self.storage.extras,
                            u16::MAX,
                            self.styles(),
                        );
                        self.storage.lazy_buffer.push_row(row, extracted);
                    }
                } else {
                    drop(self.storage.rows.drain(..from_front));
                }
            }
            if from_back > 0 {
                // Push bottom visible rows to lazy scrollback before
                // discarding (#7662). Without this, content at the bottom
                // of the screen is silently lost when the terminal height
                // shrinks. Stage while detached for an off-thread reflow too
                // (flushed on re-attach), so a mid-reflow-window height shrink
                // preserves this content (audit bug B).
                let has_scrollback = self.storage.scrollback.is_some()
                    || self.storage.scrollback_detached_for_reflow;
                if has_scrollback {
                    let start = self.storage.rows.len() - from_back;
                    // These are visible rows being pushed to scrollback due
                    // to height decrease. Their extras are still live in
                    // self.storage.extras keyed by their external row index.
                    // After linearization and front-drain, scrollback rows
                    // occupy positions 0..remaining_scrollback, so the
                    // external (visible) row index for Vec position p is
                    // p - remaining_scrollback. (#7783)
                    let remaining_scrollback = scrollback.saturating_sub(from_front);
                    let drained_rows: Vec<Row> = self.storage.rows.drain(start..).collect();
                    for (i, row) in drained_rows.iter().enumerate() {
                        let external_row = row_u16(start + i - remaining_scrollback);
                        let extracted = Self::extract_row_extras(
                            row,
                            &self.storage.extras,
                            external_row,
                            self.styles(),
                        );
                        self.storage.lazy_buffer.push_row(row, extracted);
                    }
                } else {
                    for _ in 0..from_back {
                        self.storage.rows.pop();
                    }
                }
            }
        }

        self.storage.total_lines = self.storage.rows.len();

        // The reveal accounting (see the doc above): growth beyond the
        // ring-resident rows is filled by fresh blanks below; growth within
        // them re-labels the newest ring history as visible top rows.
        let revealed = target
            .saturating_sub(old_visible)
            .min(self.storage.rows.len().saturating_sub(old_visible));

        if target > self.storage.rows.len() {
            let ring_head = self.storage.ring_head;
            if ring_head != 0 {
                self.storage.rows.rotate_left(ring_head);
                self.storage.ring_head = 0;
            }
            let rows_to_add = target - self.storage.rows.len();
            {
                let rows = &mut self.storage.rows;
                let pages = &mut self.storage.pages;
                // SAFETY: New rows are stored in the same `GridStorage` that
                // owns `pages`, and rows drop before the backing pages.
                for _ in 0..rows_to_add {
                    rows.push(unsafe { Row::new(new_cols, pages) });
                }
            }
            self.storage.total_lines += rows_to_add;
        }
        revealed
    }

    /// Rows-only [`adjust_row_count`](Self::adjust_row_count) for a RING-ONLY
    /// grid: reclassify viewport rows against ring history in place — never
    /// shed retention to fit the viewport. The retention cap is enforced like
    /// `scroll_up`'s at-capacity reuse (oldest evicted only past it), so
    /// surviving lines keep their absolute-row identity.
    ///
    /// Returns the revealed-history count (see [`Self::adjust_row_count`]).
    fn adjust_row_count_ring_only(&mut self, target: usize, new_cols: u16) -> usize {
        let visible = self.storage.visible_rows as usize;
        debug_assert_eq!(
            self.storage.total_lines,
            self.storage.rows.len(),
            "ring-only grid: every ring row is a retained line"
        );

        if target < visible {
            // SHRINK: the bottom (visible - target) viewport rows become the
            // NEWEST history (#7662's bottom-push, in ring form) and the top
            // `target` rows stay visible. Linearize, then rotate the viewport
            // slice: [hist | V0..Vv-1] -> [hist | Vt..Vv-1 | V0..Vt-1].
            if self.storage.ring_head != 0 {
                self.storage.rows.rotate_left(self.storage.ring_head);
                self.storage.ring_head = 0;
            }
            let hist = self.storage.total_lines - visible;

            // The demoted rows' live extras (keyed by their visible row, the
            // #7783 external-row rule) move into ring_extras — the ring
            // history's side table — in age order. Re-align the deque first:
            // it may be legitimately empty while history exists (the
            // post-clear steady state reuse keeps it empty), and appending
            // must not shift those default entries.
            while self.storage.ring_extras.len() < hist {
                self.storage.ring_extras.push_back(None);
            }
            for i in target..visible {
                let extracted = Self::extract_row_extras(
                    &self.storage.rows[hist + i],
                    &self.storage.extras,
                    row_u16(i),
                    self.styles(),
                );
                self.storage.push_ring_extras(extracted);
            }
            self.storage.rows[hist..].rotate_left(target);

            // Enforce the retention cap: a full ring cannot absorb the
            // demoted rows, so evict the oldest past it — the same
            // observable effect as scroll_up's at-capacity eviction. (The
            // dropped rows' page memory is reclaimed on the next rebuild,
            // like the tiered trim path.)
            let excess =
                (self.storage.total_lines - target).saturating_sub(self.storage.max_scrollback);
            if excess > 0 {
                drop(self.storage.rows.drain(..excess));
                for _ in 0..excess {
                    self.storage.ring_extras.pop_front();
                }
                self.storage.total_lines -= excess;
            }
        } else if target > visible {
            // GROW: reveal up to (target - visible) newest history lines by
            // pure reclassification — they already sit in the ring directly
            // above the viewport, so the caller's visible_rows update alone
            // re-labels them (the tiered path reveals the same rows via its
            // store round-trip). Their ring_extras entries leave the history
            // side table with them; like the tiered path, re-labelled rows
            // re-pair with live extras only where the cells still carry them.
            let hist = self.storage.total_lines - visible;
            let revealed = (target - visible).min(hist);
            let keep = hist - revealed;
            while self.storage.ring_extras.len() > keep {
                self.storage.ring_extras.pop_back();
            }
            // Any remaining growth needs fresh blank rows at the bottom.
            if target > self.storage.total_lines {
                if self.storage.ring_head != 0 {
                    self.storage.rows.rotate_left(self.storage.ring_head);
                    self.storage.ring_head = 0;
                }
                let rows_to_add = target - self.storage.total_lines;
                let rows = &mut self.storage.rows;
                let pages = &mut self.storage.pages;
                for _ in 0..rows_to_add {
                    // SAFETY: New rows are stored in the same `GridStorage`
                    // that owns `pages`, and rows drop before the backing pages.
                    rows.push(unsafe { Row::new(new_cols, pages) });
                }
                self.storage.total_lines += rows_to_add;
            }
            return revealed;
        }
        // target == visible: nothing to reclassify.
        0
    }

    /// Reflow lines when column count changes.
    fn reflow_columns(
        &mut self,
        target_rows: u16,
        new_cols: u16,
        cursor_row: usize,
        cursor_col: u16,
    ) {
        let old_extras = self
            .storage
            .extras
            .has_any_data()
            .then(|| std::mem::take(&mut self.storage.extras));
        let old_extras_ref = old_extras.as_ref();
        if new_cols > self.storage.cols {
            self.reflow_grow_columns(
                target_rows,
                new_cols,
                cursor_row,
                cursor_col,
                old_extras_ref,
            );
        } else {
            self.reflow_shrink_columns(
                target_rows,
                new_cols,
                cursor_row,
                cursor_col,
                old_extras_ref,
            );
        }
    }

    /// Pad or truncate to target row count and update grid state after reflow.
    ///
    /// When shrinking columns causes wrapping that produces more rows than
    /// `target_rows`, excess rows from the top are pushed to scrollback (lazy
    /// buffer) before truncation, preserving cursor content. (#7410)
    ///
    /// Drops old grid data before allocating padding rows so that peak memory
    /// during resize is reduced — the old page store is freed before new
    /// empty-row pages are allocated (#4074).
    fn finalize_reflow(&mut self, target_rows: u16, mut result: ReflowResult, new_cols: u16) {
        let target_rows = usize::from(target_rows);

        // If the cursor overflows the visible area, push excess top rows to
        // scrollback instead of silently discarding them (#7410).
        if result.rows.len() > target_rows && result.cursor_row >= target_rows {
            let rows_to_push = result.rows.len() - target_rows;
            // Push the minimum needed to bring cursor into the visible window.
            // This is the number of rows we need to remove from the top.
            let push_count = rows_to_push.min(result.cursor_row + 1 - target_rows);
            let push_count = push_count.min(result.rows.len().saturating_sub(target_rows));

            // Collect drained rows so we can borrow result.extras
            // for extract_row_extras while iterating (#7448).
            let drained_rows: Vec<Row> = result.rows.drain(..push_count).collect();
            for (i, row) in drained_rows.iter().enumerate() {
                let row_idx = u16::try_from(i).unwrap_or(u16::MAX);
                let extracted =
                    Self::extract_row_extras(row, &result.extras, row_idx, self.styles());
                self.storage.lazy_buffer.push_row(row, extracted);
            }

            // Shift extras row indices to match the row removal.
            if push_count > 0 {
                if let Ok(n) = u16::try_from(push_count) {
                    result.extras.shift_rows_up_by(0, n);
                }
                result.cursor_row -= push_count;
            }
        }

        result.rows.truncate(target_rows);

        // Release old grid data before padding allocation to reduce peak
        // memory. After the reflow loop the old rows/pages are unreferenced.
        drop(std::mem::take(&mut self.storage.rows));
        self.storage.pages = result.pages;

        // SAFETY: Each padding row is created against `self.storage.pages`, which
        // remains owned by `self` for the lifetime of the inserted rows.
        while result.rows.len() < target_rows {
            result
                .rows
                .push(unsafe { Row::new(new_cols, &mut self.storage.pages) });
        }

        self.storage.rows = result.rows;
        self.storage.ring_head = 0;
        self.storage.total_lines = self.storage.rows.len();
        let visible_rows = row_u16(target_rows);
        self.storage.visible_rows = visible_rows;
        self.storage.extras = result.extras;
        self.storage.extras.retain_rows_below(visible_rows);
        self.storage.sync_all_extras_flags();

        // Rescan any_double_width after reflow: double-width rows may have been
        // pushed to scrollback, making the flag stale. Without this, the flag
        // permanently degrades cursor-operation performance after any DECDWL/DECDHL
        // usage, even when no double-width rows remain in the visible area. (#7497)
        self.storage.any_double_width = self.storage.rows.iter().any(|r| {
            matches!(
                r.line_size(),
                LineSize::DoubleWidth | LineSize::DoubleHeightTop | LineSize::DoubleHeightBottom
            )
        });

        let max_row = row_u16(self.storage.rows.len().saturating_sub(1));
        self.storage.cursor.row = row_u16(result.cursor_row).min(max_row);
        self.storage.cursor.col = result.cursor_col.min(new_cols.saturating_sub(1));
    }

    /// Reflow when terminal gets wider: unwrap soft-wrapped lines.
    ///
    /// Reads row data directly from the ring buffer instead of cloning the
    /// entire visible grid. A reusable merge buffer handles continuation-row
    /// concatenation, eliminating per-logical-line `Vec` allocations (#4074).
    fn reflow_grow_columns(
        &mut self,
        target_rows: u16,
        new_cols: u16,
        cursor_row: usize,
        cursor_col: u16,
        old_extras: Option<&CellExtras>,
    ) {
        let mut new_pages = PageStore::new();
        let visible_count = usize::from(self.storage.visible_rows);
        let mut new_rows: Vec<Row> = Vec::with_capacity(visible_count);
        let mut cursor = (cursor_row, cursor_col);
        let mut merge_buf: Vec<super::Cell> = Vec::with_capacity(self.storage.cols as usize);
        let mut merge_coords: Vec<CellCoord> = Vec::new();
        let mut new_extras = CellExtras::new();

        let mut i = 0;
        while i < visible_count {
            #[cfg(any(test, feature = "testing"))]
            super::count_reflow_row_op();

            let row = match self.row(row_u16(i)) {
                Some(r) => r,
                None => {
                    i += 1;
                    continue;
                }
            };
            let content_len = row.len() as usize;
            let first_row_idx = i;
            let has_cont =
                i + 1 < visible_count && self.row(row_u16(i + 1)).is_some_and(Row::is_wrapped);

            let source_line_size = row.line_size();

            // DECDWL/DECDHL rows must NOT be reflowed — resize in place (#7524).
            if source_line_size != LineSize::SingleWidth {
                resize_double_width_row_in_place(
                    row,
                    first_row_idx,
                    new_cols,
                    &mut new_pages,
                    &mut new_rows,
                    cursor_row,
                    cursor_col,
                    &mut cursor,
                    old_extras,
                    &mut new_extras,
                );
                i += 1;
                continue;
            }

            if has_cont {
                self.merge_continuation_rows(
                    i,
                    visible_count,
                    cursor_row,
                    cursor_col,
                    &mut merge_buf,
                    &mut merge_coords,
                    &mut i,
                    new_cols,
                    &mut new_pages,
                    &mut new_rows,
                    &mut cursor,
                    old_extras,
                    &mut new_extras,
                );
            } else if content_len == 0 {
                // SAFETY: `new_row` is appended to `new_rows` and returned
                // alongside `new_pages` in the same reflow result.
                let mut new_row = unsafe { Row::new(new_cols, &mut new_pages) };
                if row.is_wrapped() {
                    new_row.set_wrapped(true);
                }
                new_row.set_line_size(source_line_size);
                if cursor_row == first_row_idx {
                    cursor = (new_rows.len(), cursor_col.min(new_cols.saturating_sub(1)));
                }
                new_rows.push(new_row);
            } else {
                let was_wrapped = row.is_wrapped();
                let first_idx = new_rows.len();
                let cells = &row.as_slice()[..content_len];
                let offset = (cursor_row == first_row_idx).then(|| usize::from(cursor_col));
                let mut extras_ctx = ExtrasCopyCtx {
                    // Single source row `i` chunked across new rows → compute coords.
                    source: if old_extras.is_some() {
                        ExtrasSource::Row(row_u16(i))
                    } else {
                        ExtrasSource::None
                    },
                    old_extras,
                    new_extras: &mut new_extras,
                };
                chunk_cells_to_rows(
                    cells,
                    new_cols,
                    &mut new_pages,
                    &mut new_rows,
                    offset,
                    &mut cursor,
                    &mut extras_ctx,
                );
                // Inherit the original row's wrapped flag on the first chunk,
                // mirroring the shrink path (line ~458). Without this, a row
                // that was a continuation of a scrollback line loses its flag
                // after grow reflow, breaking cross-boundary search/copy (#7234).
                if first_idx < new_rows.len() {
                    if was_wrapped {
                        new_rows[first_idx].set_wrapped(true);
                    }
                    new_rows[first_idx].set_line_size(source_line_size);
                }
            }
            i += 1;
        }
        self.finalize_reflow(
            target_rows,
            ReflowResult {
                rows: new_rows,
                pages: new_pages,
                extras: new_extras,
                cursor_row: cursor.0,
                cursor_col: cursor.1,
            },
            new_cols,
        );
    }

    /// Merge continuation rows into `merge_buf`, then chunk into new rows.
    ///
    /// Advances `*i` past all continuation rows consumed.
    #[allow(clippy::too_many_arguments)]
    fn merge_continuation_rows(
        &self,
        start: usize,
        visible_count: usize,
        cursor_row: usize,
        cursor_col: u16,
        merge_buf: &mut Vec<super::Cell>,
        merge_coords: &mut Vec<CellCoord>,
        i: &mut usize,
        new_cols: u16,
        new_pages: &mut PageStore,
        new_rows: &mut Vec<Row>,
        cursor: &mut (usize, u16),
        old_extras: Option<&CellExtras>,
        new_extras: &mut CellExtras,
    ) {
        merge_buf.clear();
        merge_coords.clear();
        let mut cursor_offset: Option<usize> = None;

        // Save the first row's wrapped flag and line_size before merging. If
        // this row is a continuation of a scrollback line, the flag must survive
        // the merge so search/copy across the scrollback boundary works (#7234).
        // The line_size (DECDWL/DECDHL) comes from the first source row since
        // continuation rows are always single-width.
        let first_row_was_wrapped = self.row(row_u16(start)).is_some_and(Row::is_wrapped);
        let first_row_line_size = self
            .row(row_u16(start))
            .map_or(LineSize::SingleWidth, Row::line_size);

        // Copy first row's cells.
        if let Some(row) = self.row(row_u16(start)) {
            let len = row.len() as usize;
            merge_buf.extend_from_slice(&row.as_slice()[..len]);
            if old_extras.is_some() {
                merge_coords.extend(source_coords_for_row(row_u16(start), len));
            }
        }
        if cursor_row == start {
            cursor_offset = Some(usize::from(cursor_col));
        }

        // Copy continuation rows.
        while *i + 1 < visible_count && self.row(row_u16(*i + 1)).is_some_and(Row::is_wrapped) {
            *i += 1;
            if let Some(cont) = self.row(row_u16(*i)) {
                let off = merge_buf.len();
                let len = cont.len() as usize;
                merge_buf.extend_from_slice(&cont.as_slice()[..len]);
                if old_extras.is_some() {
                    merge_coords.extend(source_coords_for_row(row_u16(*i), len));
                }
                if cursor_row == *i {
                    cursor_offset = Some(off + usize::from(cursor_col));
                }
            }
        }

        if merge_buf.is_empty() {
            // SAFETY: `new_row` is appended to `new_rows` and returned
            // alongside `new_pages` in the same reflow result.
            let mut new_row = unsafe { Row::new(new_cols, new_pages) };
            new_row.set_line_size(first_row_line_size);
            if cursor_offset.is_some() {
                *cursor = (new_rows.len(), cursor_col.min(new_cols.saturating_sub(1)));
            }
            new_rows.push(new_row);
        } else {
            let first_idx = new_rows.len();
            let mut extras_ctx = ExtrasCopyCtx {
                // MERGE path: cells span several source rows, so the explicit
                // (reused) coord table is required — not pure row arithmetic.
                source: if old_extras.is_some() {
                    ExtrasSource::Coords(merge_coords.as_slice())
                } else {
                    ExtrasSource::None
                },
                old_extras,
                new_extras,
            };
            chunk_cells_to_rows(
                merge_buf,
                new_cols,
                new_pages,
                new_rows,
                cursor_offset,
                cursor,
                &mut extras_ctx,
            );
            // Inherit the first merge row's wrapped flag and line_size on the
            // first output chunk — same pattern as shrink reflow and non-merge
            // grow (#7234). Line size (DECDWL/DECDHL) from first source row.
            if first_idx < new_rows.len() {
                if first_row_was_wrapped {
                    new_rows[first_idx].set_wrapped(true);
                }
                new_rows[first_idx].set_line_size(first_row_line_size);
            }
        }
    }

    /// Reflow when terminal gets narrower: wrap long lines.
    ///
    /// Reads cell data directly from the ring buffer via row slices, avoiding
    /// the full-grid clone that `collect_visible_rows()` previously performed
    /// (#4074).
    fn reflow_shrink_columns(
        &mut self,
        target_rows: u16,
        new_cols: u16,
        cursor_row: usize,
        cursor_col: u16,
        old_extras: Option<&CellExtras>,
    ) {
        let mut new_pages = PageStore::new();
        let visible_count = usize::from(self.storage.visible_rows);
        let est_rows = visible_count
            .saturating_mul(self.storage.cols as usize)
            .checked_div(new_cols as usize)
            .unwrap_or(visible_count);
        let mut new_rows: Vec<Row> = Vec::with_capacity(est_rows.min(visible_count * 4));
        let mut cursor = (cursor_row, cursor_col);
        let mut new_extras = CellExtras::new();

        for i in 0..visible_count {
            let row = match self.row(row_u16(i)) {
                Some(r) => r,
                None => continue,
            };
            let was_wrapped = row.is_wrapped();
            let source_line_size = row.line_size();
            let content_len = row.len() as usize;

            #[cfg(any(test, feature = "testing"))]
            super::count_reflow_row_op();

            // DECDWL/DECDHL: resize in place, not reflow (#7524).
            if source_line_size != LineSize::SingleWidth {
                resize_double_width_row_in_place(
                    row,
                    i,
                    new_cols,
                    &mut new_pages,
                    &mut new_rows,
                    cursor_row,
                    cursor_col,
                    &mut cursor,
                    old_extras,
                    &mut new_extras,
                );
                continue;
            }

            if content_len == 0 {
                // SAFETY: `new_row` is appended to `new_rows` and returned
                // alongside `new_pages` in the same reflow result.
                let mut new_row = unsafe { Row::new(new_cols, &mut new_pages) };
                if was_wrapped {
                    new_row.set_wrapped(true);
                }
                new_row.set_line_size(source_line_size);
                if i == cursor_row {
                    cursor = (new_rows.len(), cursor_col.min(new_cols.saturating_sub(1)));
                }
                new_rows.push(new_row);
                continue;
            }

            let first_idx = new_rows.len();
            let cells = &row.as_slice()[..content_len];
            let offset = (i == cursor_row).then(|| usize::from(cursor_col));
            let mut extras_ctx = ExtrasCopyCtx {
                // Single source row `i` chunked across new rows → compute coords.
                source: if old_extras.is_some() {
                    ExtrasSource::Row(row_u16(i))
                } else {
                    ExtrasSource::None
                },
                old_extras,
                new_extras: &mut new_extras,
            };
            chunk_cells_to_rows(
                cells,
                new_cols,
                &mut new_pages,
                &mut new_rows,
                offset,
                &mut cursor,
                &mut extras_ctx,
            );
            // Inherit the original row's wrapped flag and line_size on the
            // first chunk. Line size (DECDWL/DECDHL) from the source row.
            if first_idx < new_rows.len() {
                if was_wrapped {
                    new_rows[first_idx].set_wrapped(true);
                }
                new_rows[first_idx].set_line_size(source_line_size);
            }
        }

        self.finalize_reflow(
            target_rows,
            ReflowResult {
                rows: new_rows,
                pages: new_pages,
                extras: new_extras,
                cursor_row: cursor.0,
                cursor_col: cursor.1,
            },
            new_cols,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // ReflowMode conversion
    // =========================================================================

    #[test]
    fn reflow_mode_from_true() {
        assert_eq!(ReflowMode::from(true), ReflowMode::Enabled);
    }

    #[test]
    fn reflow_mode_from_false() {
        assert_eq!(ReflowMode::from(false), ReflowMode::Disabled);
    }

    #[test]
    fn reflow_mode_debug_repr() {
        // Verify Debug is derived and produces expected output.
        let enabled = format!("{:?}", ReflowMode::Enabled);
        let disabled = format!("{:?}", ReflowMode::Disabled);
        assert!(enabled.contains("Enabled"));
        assert!(disabled.contains("Disabled"));
    }

    #[test]
    fn reflow_mode_clone_eq() {
        let mode = ReflowMode::Enabled;
        let cloned = mode;
        assert_eq!(mode, cloned);
    }

    // =========================================================================
    // Resize dimension bounds (§5.8 ingress clamp)
    // =========================================================================

    #[test]
    fn resize_clamps_oversize_dimensions() {
        let mut grid = Grid::new(5, 10);
        grid.resize(u16::MAX, u16::MAX);
        assert_eq!(grid.rows(), MAX_GRID_ROWS);
        assert_eq!(grid.cols(), MAX_GRID_COLS);
        grid.assert_invariants();
    }

    #[test]
    fn resize_no_reflow_clamps_oversize_dimensions() {
        let mut grid = Grid::new(5, 10);
        grid.resize_no_reflow(u16::MAX, u16::MAX);
        assert_eq!(grid.rows(), MAX_GRID_ROWS);
        assert_eq!(grid.cols(), MAX_GRID_COLS);
        grid.assert_invariants();
    }

    // =========================================================================
    // Grid-level reflow: narrower -> wider -> same width
    // =========================================================================

    #[test]
    fn reflow_same_width_is_identity() {
        let mut grid = Grid::new(5, 10);
        for c in "ABCDEFGHIJ".chars() {
            grid.write_char(c);
        }
        grid.set_cursor(0, 5);

        // Resize to same width: no reflow should occur.
        grid.resize(5, 10);

        assert_eq!(grid.row(0).unwrap().to_string(), "ABCDEFGHIJ");
        assert_eq!(grid.cursor_col(), 5);
        assert_eq!(grid.cursor_row(), 0);
        grid.assert_invariants();
    }

    #[test]
    fn reflow_shrink_single_char_line() {
        let mut grid = Grid::new(3, 10);
        grid.write_char('X');
        grid.set_cursor(0, 0);

        grid.resize(3, 5);

        assert_eq!(grid.row(0).unwrap().to_string(), "X");
        assert_eq!(grid.cursor_row(), 0);
        assert_eq!(grid.cursor_col(), 0);
        grid.assert_invariants();
    }

    #[test]
    fn reflow_shrink_to_1_column() {
        let mut grid = Grid::new(5, 4);
        for c in "ABCD".chars() {
            grid.write_char(c);
        }
        grid.set_cursor(0, 0);

        grid.resize(5, 1);

        // Each character should end up on its own row.
        assert_eq!(grid.row(0).unwrap().to_string(), "A");
        assert_eq!(grid.row(1).unwrap().to_string(), "B");
        assert_eq!(grid.row(2).unwrap().to_string(), "C");
        assert_eq!(grid.row(3).unwrap().to_string(), "D");
        // Rows 1-3 should be wrapped continuations.
        assert!(grid.row(1).unwrap().is_wrapped());
        assert!(grid.row(2).unwrap().is_wrapped());
        assert!(grid.row(3).unwrap().is_wrapped());
        grid.assert_invariants();
    }

    #[test]
    fn reflow_grow_from_1_column() {
        let mut grid = Grid::new(5, 4);
        for c in "ABCD".chars() {
            grid.write_char(c);
        }

        // Shrink to 1 col then grow back.
        grid.resize(5, 1);
        grid.resize(5, 4);

        assert_eq!(grid.row(0).unwrap().to_string(), "ABCD");
        grid.assert_invariants();
    }

    #[test]
    fn reflow_shrink_multiple_lines() {
        let mut grid = Grid::new(5, 10);
        // Line 0: "ABCDEFGHIJ"
        for c in "ABCDEFGHIJ".chars() {
            grid.write_char(c);
        }
        grid.line_feed();
        grid.carriage_return();
        // Line 1: "12345"
        for c in "12345".chars() {
            grid.write_char(c);
        }

        grid.resize(5, 5);

        // Line 0 splits into 2 rows, Line 1 fits in 1 row.
        assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
        assert_eq!(grid.row(1).unwrap().to_string(), "FGHIJ");
        assert_eq!(grid.row(2).unwrap().to_string(), "12345");
        assert!(grid.row(1).unwrap().is_wrapped());
        assert!(!grid.row(2).unwrap().is_wrapped());
        grid.assert_invariants();
    }

    #[test]
    fn reflow_grow_merges_only_soft_wrapped() {
        let mut grid = Grid::new(5, 5);
        // Write "ABCDE" on row 0.
        for c in "ABCDE".chars() {
            grid.write_char(c);
        }
        // Hard line break.
        grid.line_feed();
        grid.carriage_return();
        // Write "12345" on row 1.
        for c in "12345".chars() {
            grid.write_char(c);
        }

        // Neither row is wrapped (hard breaks). Growing should NOT merge them.
        grid.resize(5, 20);

        assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
        assert_eq!(grid.row(1).unwrap().to_string(), "12345");
        grid.assert_invariants();
    }

    #[test]
    fn reflow_cursor_tracking_through_shrink_grow_roundtrip() {
        let mut grid = Grid::new(5, 10);
        for c in "ABCDEFGHIJ".chars() {
            grid.write_char(c);
        }
        grid.set_cursor(0, 7); // on 'H'

        grid.resize(5, 5);
        // After shrink: "ABCDE" on row 0, "FGHIJ" on row 1.
        // Cursor was at logical offset 7 -> row 1, col 2.
        assert_eq!(grid.cursor_row(), 1);
        assert_eq!(grid.cursor_col(), 2);

        grid.resize(5, 10);
        // After grow: "ABCDEFGHIJ" on row 0.
        // Cursor should map back to row 0, col 7.
        assert_eq!(grid.cursor_row(), 0);
        assert_eq!(grid.cursor_col(), 7);
        grid.assert_invariants();
    }

    #[test]
    fn reflow_disabled_does_not_wrap() {
        let mut grid = Grid::new(5, 10);
        for c in "ABCDEFGHIJ".chars() {
            grid.write_char(c);
        }

        grid.resize_with_reflow_mode(5, 5, ReflowMode::Disabled);

        // Content truncated, not wrapped.
        assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
        assert!(grid.row(1).unwrap().is_empty());
        grid.assert_invariants();
    }

    #[test]
    fn reflow_disabled_grow_does_not_unwrap() {
        let mut grid = Grid::new(5, 5);
        for c in "ABCDE".chars() {
            grid.write_char(c);
        }
        grid.line_feed();
        grid.carriage_return();
        if let Some(row) = grid.row_mut(1) {
            row.set_wrapped(true);
            for (i, c) in "FGHIJ".chars().enumerate() {
                row.write_char(i as u16, c);
            }
        }

        // Growing with reflow disabled should NOT unwrap.
        grid.resize_with_reflow_mode(5, 20, ReflowMode::Disabled);

        // Rows should remain separate (no merge).
        assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
        assert_eq!(grid.row(1).unwrap().to_string(), "FGHIJ");
        grid.assert_invariants();
    }
}
