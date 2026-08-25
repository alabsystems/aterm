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
use super::scroll_convert::ScrolledRowExtras;
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

/// May the rewrap merge CONSUME `row` as a soft-wrap continuation of the
/// preceding row? Only a single-width continuation merges: a DECDWL/DECDHL row
/// carries a per-row `line_size` and must resize IN PLACE
/// (`resize_double_width_row_in_place`, #7524) even when it is itself a wrap
/// continuation — merging it would strip its DoubleWidth attribute (the merge
/// buffer holds bare cells, and the output chunk inherits only the FIRST
/// source row's line_size).
fn is_mergeable_continuation(row: &Row) -> bool {
    row.is_wrapped() && row.line_size() == LineSize::SingleWidth
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
        // The ABSOLUTE row under the eye, captured before the line above destroys
        // the offset it is derived from. Only meaningful while the reader is
        // actually scrolled back: at offset 0 the anchor is the live top, and a
        // height SHRINK legitimately wants `d + (v - t) > 0` for it — i.e. it
        // would scroll a live, tail-following viewport into history on every
        // window-height drag. The gate is what keeps the tail follower live, and
        // it is also why the resize fuzzers (which never scroll) never reach the
        // anchor arm below.
        let prev_anchor = (prev_offset > 0).then(|| self.top_visible_absolute_row());
        self.storage.display_offset = 0;

        // Bottom-anchor bookkeeping (fixwave5): remember how many trailing
        // blank rows the viewport carried BEFORE the resize. A width-grow
        // unwraps content into fewer rows; the deficit fill at the end of this
        // method pulls history back until the trailing-blank band returns to
        // this count, so the prompt stays anchored to its content instead of
        // stranding mid-window above a band of reflow-created blanks.
        // (Rows-only resizes deliberately do NOT ride this fill: the grow
        // reveal is a pure relabel that keeps absolute numbering, which the
        // fill's history renumbering would break for the anchored reader.)
        let pre_trailing_blanks =
            (new_cols != old_cols && reflow).then(|| self.trailing_blank_rows_below_cursor());

        // On a width change with reflow, lift the entire off-screen scrollback
        // out and rewrap it to the new width BEFORE any visible-grid mutation,
        // so history survives the resize (#7906). Reads ring_extras, so it must
        // precede the ring_extras.clear() below. Restored after adjust_row_count.
        let reflowed_scrollback = if new_cols != old_cols && reflow {
            let mut old = self.take_scrollback_lines();
            // Lift the viewport's leading soft-wrap continuation rows (the
            // tail of a logical line whose HEAD is the last history line) into
            // the history rewrap, so the boundary-straddling line rewraps as
            // ONE unit instead of splitting permanently at the seam — the
            // audit's "wrapped line stays split after returning to the
            // original width" (fixwave5). The deficit fill below pulls the
            // rewrapped tail back into the viewport.
            old.extend(self.take_boundary_continuation_lines());
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
        // (#4149, #4215) — but a ROWS-ONLY resize never rebuilds the ring on
        // ANY grid shape (history is reclassified in place, see
        // `adjust_row_count_rows_only`), so its history extras must survive.
        // This gate used to also require a ring-only grid, which is what forced
        // the tiered path to evacuate the whole ring in order to have somewhere
        // for the extras to live: with `ring_extras` cleared, ring history rows
        // could no longer carry their hyperlink/RGB entries, so the only way to
        // keep them was to materialize every row into the store. Keeping the
        // side table is what makes the in-place path legal for a tiered grid.
        let rows_only = new_cols == old_cols;
        if !rows_only {
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

        let (revealed, reveal_extras) = self.adjust_row_count(new_rows, new_cols);
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
            // The live extras follow their rows down too (fixwave5): without
            // this, a hyperlink/RGB/combining entry stays keyed `revealed`
            // rows ABOVE its cell and re-attaches to whatever content the
            // reveal placed there. (The rings were invalidated above, so this
            // shifts only the HashMap.)
            self.storage
                .extras
                .shift_region_down_by(0, bound, row_u16(revealed));
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
        // Re-attach the revealed ring rows' extracted extras at their FINAL
        // viewport rows — after the shift above, so they cannot ride it (see
        // `adjust_row_count_rows_only`; discarding these is how emoji came
        // back as U+FFFD and hyperlinks vanished across a shrink+grow).
        for (row, bx) in reveal_extras {
            self.inject_scrolled_extras(row, &bx);
        }
        // Restore the rewrapped history as the front (oldest) of the scrollback,
        // after the visible grid is finalized so adjust_row_count cannot trim it
        // and the new dimensions are in place (#7906).
        if let Some(lines) = reflowed_scrollback {
            self.restore_reflowed_scrollback(lines, new_cols);
        }
        // Bottom-anchor the viewport (fixwave5): a width-grow just unwrapped
        // content into fewer rows, leaving reflow-created blank rows under the
        // cursor. Pull the newest history back in until the trailing-blank
        // band matches its pre-resize count — this is also what rejoins the
        // boundary-straddling logical line the belt lift above handed to the
        // history rewrap. Runs at display_offset 0 (restored just below); on
        // the OFFLOADED path the history is out with the worker, so the fill
        // finds nothing here and runs at re-attach instead (see
        // `pending_fill_target`).
        if let Some(target) = pre_trailing_blanks {
            self.fill_viewport_deficit_from_history(target);
        }
        // Restore the pre-resize reading position. `display_offset` is measured
        // from the LIVE BOTTOM, so replaying the same number under a different
        // `visible_rows` slides the content by exactly the row-count delta — and
        // window-height drags, font zoom and divider drags are ALL rows-only
        // resizes, so that fired on every one of them. A rows-only resize
        // rewraps nothing and leaves `absolute_row_counter` alone; it only
        // re-splits the same lines across the live/history boundary, and an
        // ALREADY-ARCHIVED line keeps its absolute number exactly — a shrink
        // trims blanks and demotes TOP viewport rows into history (with the
        // bottom-push corner, see `adjust_row_count_rows_only`), and a grow
        // appends blanks whose scrolled-back arm pulls nothing (the deficit
        // fill is gated to `prev_offset == 0` precisely so the anchor below
        // stays exact); everything deeper is untouched in both. `prev_offset >
        // 0` means the row under the eye IS such a line, which is what makes
        // this anchor exact rather than approximate. A WIDTH reflow renumbers
        // rows wholesale (the wrapped-line count changes), so no exact anchor
        // exists there and the clamped offset stays the best available answer —
        // staying in history is far better than snapping a scrolled-back reader
        // to the live bottom.
        //
        // The anchor arm can still CLAMP: on a rows shrink at the retention cap
        // the demanded history may exceed the post-resize `scrollback_lines()`.
        // That moves the reader off the anchored line but stays in bounds — the
        // same degradation the offset arm has always had. `Damage::Full` below
        // subsumes the primitive's targeted damage.
        match prev_anchor {
            Some(anchor) if new_cols == old_cols => self.scroll_to_absolute_row(anchor),
            _ => self.storage.display_offset = prev_offset.min(self.storage.scrollback_lines()),
        }
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
    /// count — the cursor's content moved that many rows down the viewport —
    /// and owes the returned `(viewport row, extras)` pairs re-injection into
    /// the live map AFTER that shift (see `adjust_row_count_rows_only`).
    fn adjust_row_count(
        &mut self,
        target_rows: u16,
        new_cols: u16,
    ) -> (usize, Vec<(u16, Box<ScrolledRowExtras>)>) {
        let target = target_rows as usize;
        let old_visible = usize::from(self.storage.visible_rows);

        // A ROWS-ONLY resize must not shed ring history to fit the viewport,
        // on ANY grid shape. Reclassify viewport rows against ring history in
        // place instead of the store migration below. Identity law: same
        // logical buffer (history sequence, viewport, absolute numbering) —
        // `scrollback_lines()` counts a retained line the same whether it sits
        // in the ring, the lazy buffer or the store, so relocating the ring
        // into the store expresses nothing the in-place reclassification does
        // not. Width changes keep the pre-existing machinery (their ring
        // history rides take_scrollback_lines / restore, and this method then
        // runs against reflow_columns' freshly rebuilt rows).
        //
        // `self.storage.cols` is still the PRE-resize width here:
        // `resize_viewport_state` installs `new_cols` only after this returns.
        if new_cols == self.storage.cols {
            return self.adjust_row_count_rows_only(target, new_cols);
        }

        if self.storage.rows.len() > target {
            // Bounded-cost obligation, rows-only half (see
            // `tests/reflow/rows_only_cost_bound.rs`). Unreachable while the
            // early return above stands — a rows-only resize never enters this
            // branch — which is exactly what makes it teeth: a regression that
            // routes rows-only work back through the whole-ring migration
            // lights this counter up with a history-sized number.
            #[cfg(any(test, feature = "testing"))]
            if new_cols == self.storage.cols {
                super::count_rows_only_resize_migrated_rows(self.storage.rows.len() - target);
            }
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
        // The width path's revealed rows carry no ring_extras hand-off: their
        // extras (if any) already rode the take/restore scrollback round trip.
        (revealed, Vec::new())
    }

    /// Rows-only [`adjust_row_count`](Self::adjust_row_count), for EVERY grid
    /// shape: reclassify viewport rows against ring history in place — never
    /// shed retention to fit the viewport. The retention cap is enforced like
    /// `scroll_up`'s at-capacity reuse (oldest evicted only past it), so
    /// surviving lines keep their absolute-row identity.
    ///
    /// WHY THIS IS LEGAL ON A TIERED GRID. A rows-only resize changes no line's
    /// WIDTH, so no line's wrap topology moves: the only thing that changes is
    /// where the live/history boundary falls inside the SAME ring. The tiered
    /// path used to express that by draining the whole ring into the store,
    /// because it compared the FULL ring length against the new VISIBLE row
    /// target — at the GUI's 10,000-line ring that is ~9,999 rows materialized
    /// per pane per event, synchronously, under the caller's lock, on every
    /// window-height drag / pane split / divider drag / find-bar toggle. It
    /// bought nothing: `scrollback_lines() == ring + lazy + tiered` counts a
    /// retained line identically in whichever tier it sits, so the migration
    /// was pure relocation of an unchanged logical buffer. It also STRANDED the
    /// evacuated rows' page bytes — `PageStore` is bump-only with no free path
    /// (`alloc_slice_impl`), and this path never rebuilds, so the 9,999 dropped
    /// `Row`s' pages were unreclaimable for the process lifetime while the ring
    /// re-allocated the same bytes as output refilled it.
    ///
    /// COST: O(|Δrows|), INDEPENDENT of ring depth. The only rows that leave
    /// the ring are the ones the shrunken retention cap can no longer hold — at
    /// most `visible - target` of them — and on a tiered grid those are STAGED
    /// into the lazy buffer, exactly like `scroll_up`'s at-capacity eviction,
    /// so retention past the ring is unchanged.
    ///
    /// Returns the revealed-history count plus the revealed rows' extracted
    /// extras for caller-side re-injection (see [`Self::adjust_row_count`]).
    ///
    /// ANCHORING (audit-2 item 1). The original in-place shapes were mutually
    /// inconsistent: shrink demoted the BOTTOM viewport rows as newest history
    /// (#7662's bottom-push, in ring form) while grow revealed newest history
    /// at the TOP — so every shrink+grow cycle ROTATED the screen (bottom rows
    /// came back on top), walked the prompt down, detached the cursor from its
    /// line, and corrupted scrollback reading order; the grow also DISCARDED
    /// the revealed rows' `ring_extras` (emoji/hyperlink/RGB lost in transit,
    /// a discard dating to 8a227e9b that this path made reachable for every
    /// grid shape). The shapes now anchor like every other terminal:
    ///
    /// * SHRINK first TRIMS trailing blank rows below the cursor (they carry
    ///   nothing — dropping them archives no fake history), then demotes TOP
    ///   rows into the ring (pure relabel — the exact inverse of the grow
    ///   reveal, so shrink+grow is identity), and only when the cursor sits
    ///   too high for that (a full screen with the cursor near the top) falls
    ///   back to the old bottom-push for the remainder — content-preserving,
    ///   with the old ordering quirk confined to that corner.
    /// * GROW keeps the reveal-at-top relabel (absolute numbering intact, so
    ///   a scrolled-back reader's anchor stays exact) and hands the revealed
    ///   rows' `ring_extras` back for re-injection instead of discarding
    ///   them.
    fn adjust_row_count_rows_only(
        &mut self,
        target: usize,
        new_cols: u16,
    ) -> (usize, Vec<(u16, Box<ScrolledRowExtras>)>) {
        let visible = self.storage.visible_rows as usize;
        debug_assert_eq!(
            self.storage.total_lines,
            self.storage.rows.len(),
            "rows-only resize: every ring row is a retained line"
        );

        if target < visible {
            let shrink = visible - target;
            if self.storage.ring_head != 0 {
                self.storage.rows.rotate_left(self.storage.ring_head);
                self.storage.ring_head = 0;
            }

            // 1) TRIM: trailing blank rows strictly below the cursor are not
            // content — archiving them would manufacture blank history that a
            // later grow reveals ABOVE real content. Drop them outright.
            let trim = shrink.min(self.trailing_blank_rows_below_cursor());
            if trim > 0 {
                let keep = self.storage.total_lines - trim;
                self.storage.rows.truncate(keep);
                self.storage.total_lines = keep;
                // Their extras keys land >= `target` after the demote shift
                // below and are swept by the caller's `retain_rows_below`.
            }
            let remaining = shrink - trim;

            // 2) TOP-DEMOTE: the top `demote` viewport rows become the newest
            // history — a pure relabel (they already sit directly above the
            // surviving viewport in the linearized ring), the exact inverse
            // of the grow-side pull. Capped at the cursor row so the cursor's
            // line always stays visible.
            let cursor_row = usize::from(self.storage.cursor.row).min(visible - 1);
            let demote = remaining.min(cursor_row);
            let hist = self.storage.total_lines - (visible - trim);
            if demote > 0 {
                // The demoted rows' live extras (keyed by their visible row,
                // the #7783 external-row rule) move into ring_extras — the
                // ring history's side table — in age order. Re-align the
                // deque first: it may be legitimately empty while history
                // exists (the post-clear steady-state reuse keeps it empty),
                // and appending must not shift those default entries.
                while self.storage.ring_extras.len() < hist {
                    self.storage.ring_extras.push_back(None);
                }
                for i in 0..demote {
                    let extracted = Self::extract_row_extras(
                        &self.storage.rows[hist + i],
                        &self.storage.extras,
                        row_u16(i),
                        self.styles(),
                    );
                    self.storage.push_ring_extras(extracted);
                }
                // Surviving-viewport extras and the cursor follow their rows
                // up; a demote-displaced selection is invalidated rather than
                // silently re-attached to shifted rows.
                let old_bottom = row_u16(visible - trim - 1);
                self.storage
                    .extras
                    .shift_region_up_by(0, old_bottom, row_u16(demote));
                self.storage.cursor.row = self.storage.cursor.row.saturating_sub(row_u16(demote));
                if self.storage.saved_cursor.valid {
                    self.storage.saved_cursor.cursor.row = self
                        .storage
                        .saved_cursor
                        .cursor
                        .row
                        .saturating_sub(row_u16(demote));
                }
            }
            let bottom_push = remaining - demote;
            // SELECTION CUSTODY — which of the two shapes above just ran decides
            // whether a selection can FOLLOW its content or must be destroyed.
            //
            // A pure TOP-DEMOTE is a relabel: every row, live and already-archived,
            // moves by exactly `demote`, so the selection is remappable and
            // `TextSelection::adjust_for_rows_shrink` does it. Destroying it here
            // would throw away a highlight whose text is still on screen, one row up
            // — the ordinary window-height drag, and the failure this design exists
            // to prevent. Hosts caching grid COORDINATES must still re-translate,
            // which is what `invalidate_host_coordinates` says without also claiming
            // the content is gone.
            //
            // The BOTTOM-PUSH corner is different in kind: it rotates the bottom
            // rows below the existing history, so the map is non-monotonic and a
            // span crossing the cut has no correct image. There, invalidation IS the
            // honest answer.
            //
            // `last_resize_row_shift` carries `demote` rather than `visible - target`
            // because TRIM discards blank rows without moving anything: a shrink that
            // only drops trailing blanks moves the selection by ZERO, and a delta of
            // `shrink` would push every anchor off its content.
            self.storage.last_resize_row_shift = row_u16(demote);
            if bottom_push > 0 {
                self.force_selection_invalidation();
            } else if demote > 0 {
                self.invalidate_host_coordinates();
            }

            // 3) BOTTOM-PUSH CORNER: the cursor sits too near the top for the
            // demand (a full non-blank screen, cursor high — a TUI shape).
            // Preserve the content by pushing the bottom rows as newest
            // history, the pre-rework mechanism: reading order above the
            // viewport is imperfect here, but nothing is lost, and the demote
            // above has already pinned the cursor's line on screen.
            if bottom_push > 0 {
                let hist_after_demote = hist + demote;
                // Extras of the pushed rows, keyed by their CURRENT visible
                // rows (post-demote-shift): the pushed rows are the bottom
                // `bottom_push` of the surviving viewport.
                let surviving = visible - trim - demote;
                while self.storage.ring_extras.len() < hist_after_demote {
                    self.storage.ring_extras.push_back(None);
                }
                for i in target..surviving {
                    let extracted = Self::extract_row_extras(
                        &self.storage.rows[hist_after_demote + i],
                        &self.storage.extras,
                        row_u16(i),
                        self.styles(),
                    );
                    self.storage.push_ring_extras(extracted);
                }
                self.storage.rows[hist_after_demote..].rotate_left(target);
            }

            // 4) RETENTION CAP: a full ring cannot absorb the demoted rows, so
            // evict the oldest past it — the same observable effect as
            // scroll_up's at-capacity eviction. Bounded by the height delta:
            // `total_lines <= visible + max_scrollback` on entry, so
            // `excess <= visible - target`. (The dropped rows' page memory is
            // reclaimed on the next rebuild, like the tiered trim path.)
            let excess =
                (self.storage.total_lines - target).saturating_sub(self.storage.max_scrollback);
            if excess > 0 {
                // Bounded-cost obligation, rows-only half: the rows that
                // actually leave the ring on a rows-only resize. Must stay
                // O(height delta), never O(history) — see
                // `tests/reflow/rows_only_cost_bound.rs`.
                #[cfg(any(test, feature = "testing"))]
                super::count_rows_only_resize_migrated_rows(excess);
                // A tiered store (or one detached for an off-thread reflow)
                // keeps retention past the ring, so the evicted rows are
                // STAGED into the lazy buffer rather than dropped — the same
                // hand-off `scroll_up`'s at-capacity reuse performs, with the
                // extras moved through their box (`push_row_boxed`) instead of
                // being re-extracted. Without a store there is nowhere for them
                // to go and the ring cap IS the retention limit, so they drop.
                if self.storage.stages_evicted_rows() {
                    for i in 0..excess {
                        let extras = self.storage.ring_extras.pop_front().flatten();
                        let storage = &mut self.storage;
                        storage.lazy_buffer.push_row_boxed(&storage.rows[i], extras);
                    }
                } else {
                    for _ in 0..excess {
                        self.storage.ring_extras.pop_front();
                    }
                }
                drop(self.storage.rows.drain(..excess));
                self.storage.total_lines -= excess;
            }
        } else if target > visible {
            // GROW: reveal up to (target - visible) newest history lines by
            // pure reclassification — they already sit in the ring directly
            // above the viewport, so the caller's visible_rows update alone
            // re-labels them, absolute numbering intact (which is what keeps
            // a scrolled-back reader's anchor exact). Their `ring_extras`
            // entries are handed BACK to the caller for re-injection at the
            // rows' final viewport coordinates — this pop used to DISCARD
            // them (audit-2 item 6: emoji revealed as U+FFFD, hyperlinks and
            // RGB gone; a discard dating to 8a227e9b that the shape-unified
            // routing made reachable for every grid). The hand-off exists
            // because injection must land AFTER the caller shifts the old
            // viewport's extras down by `revealed` — injected here, the
            // entries would ride that shift to the wrong rows.
            let hist = self.storage.total_lines - visible;
            let revealed = (target - visible).min(hist);
            let keep = hist - revealed;
            let mut reveal_extras: Vec<(u16, Box<ScrolledRowExtras>)> = Vec::new();
            // Deque tail = newest ring row = the BOTTOM revealed viewport row
            // (`revealed - 1`); the deque may hold fewer entries than history
            // rows (older rows without extras), which the `keep` bound
            // tolerates exactly as the old consume loop did.
            for j in (0..revealed).rev() {
                if self.storage.ring_extras.len() <= keep {
                    break;
                }
                if let Some(bx) = self.storage.ring_extras.pop_back().flatten() {
                    reveal_extras.push((row_u16(j), bx));
                }
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
            return (revealed, reveal_extras);
        }
        // target == visible: nothing to reclassify.
        (0, Vec::new())
    }

    /// The write-side inverse of [`Self::extract_row_extras`]: re-attach a
    /// revealed ring row's extracted extras to its viewport row in the LIVE
    /// map. Cells were never touched in transit (demote and reveal are pure
    /// relabels), so only the side-table entries need reseating — the cell's
    /// own `is_complex`/RGB-overflow markers still point here.
    fn inject_scrolled_extras(&mut self, row_idx: u16, e: &ScrolledRowExtras) {
        let extras = &mut self.storage.extras;
        for span in &e.hyperlinks {
            for col in span.start_col..span.end_col {
                let cell = extras.get_or_create(CellCoord::new(row_idx, col));
                cell.set_hyperlink(Some(span.url.clone()));
                cell.set_hyperlink_id(span.id.clone());
            }
        }
        for (col, s) in &e.complex_chars {
            extras
                .get_or_create(CellCoord::new(row_idx, *col))
                .set_complex_char(Some(s.clone()));
        }
        for (col, marks) in &e.combining {
            let cell = extras.get_or_create(CellCoord::new(row_idx, *col));
            for c in marks {
                cell.add_combining(*c);
            }
        }
        for (col, rgb) in &e.rgb_fg {
            extras
                .get_or_create(CellCoord::new(row_idx, *col))
                .set_fg_rgb(Some(*rgb));
        }
        for (col, rgb) in &e.rgb_bg {
            extras
                .get_or_create(CellCoord::new(row_idx, *col))
                .set_bg_rgb(Some(*rgb));
        }
        for (col, packed) in &e.underline_colors {
            extras
                .get_or_create(CellCoord::new(row_idx, *col))
                .set_underline_color_u32(Some(*packed));
        }
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
        self.reflow_rewrap_columns(target_rows, new_cols, cursor_row, cursor_col, old_extras_ref);
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

    /// Rewrap the visible rows to a new column count, in EITHER direction.
    ///
    /// Soft-wrapped continuation runs are merged into their logical line first
    /// and then re-chunked at the new width — for a shrink as well as a grow.
    /// The shrink path used to chunk each PHYSICAL row separately, which left
    /// a run of `old_cols`-sized fragments each split at `new_cols` (ragged
    /// `24,6,24,6,…` rows instead of the canonical `24,24,…`): the audit's
    /// "stacked mid-resize tails", and the seed of the permanent wrap-topology
    /// corruption a width sweep left behind (fixwave5).
    ///
    /// Reads row data directly from the ring buffer instead of cloning the
    /// entire visible grid. A reusable merge buffer handles continuation-row
    /// concatenation, eliminating per-logical-line `Vec` allocations (#4074).
    fn reflow_rewrap_columns(
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
            let has_cont = i + 1 < visible_count
                && self
                    .row(row_u16(i + 1))
                    .is_some_and(is_mergeable_continuation);

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
        let old_cols = usize::from(self.storage.cols);
        let mut row_start = merge_buf.len();
        let mut row_idx = start;
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
        while *i + 1 < visible_count
            && self
                .row(row_u16(*i + 1))
                .is_some_and(is_mergeable_continuation)
        {
            *i += 1;
            // Each merged continuation row is a real O(cols) unit of work — count
            // it so the `reflow_linear_time*` cost oracle sees per-row cost even
            // when a whole screen is one logical line.
            #[cfg(any(test, feature = "testing"))]
            super::count_reflow_row_op();
            // The row just appended CONTINUES onto this one, so autowrap
            // filled it to its last column — its trailing blank cells are real
            // content. Pad the merge buffer to the full old width, or a width
            // sweep erodes one mid-line space per chunk boundary (fixwave5).
            // EXCEPT when the continuation opens with a WIDE cell: a wide char
            // that cannot start at the last column EARLY-WRAPS, leaving that
            // cell UNWRITTEN — padding it here would materialize a phantom
            // space inside the logical line, right before the wide char.
            let early_wrap_hole = self
                .row(row_u16(*i))
                .and_then(|cont| cont.as_slice().first())
                .is_some_and(super::Cell::is_wide);
            // The hole is EXACTLY one cell — a width-2 glyph early-wraps only
            // when precisely one column remains — so pad real trimmed spaces
            // up to it rather than dropping the whole autowrap fill.
            let pad_to = row_start + old_cols - usize::from(early_wrap_hole);
            while merge_buf.len() < pad_to {
                if old_extras.is_some() {
                    merge_coords.push(CellCoord::new(
                        row_u16(row_idx),
                        row_u16(merge_buf.len() - row_start),
                    ));
                }
                merge_buf.push(super::Cell::EMPTY);
            }
            if let Some(cont) = self.row(row_u16(*i)) {
                let off = merge_buf.len();
                row_start = off;
                row_idx = *i;
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
