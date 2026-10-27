// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Grid reflow (`O(rows × cols)` line rewrapping when terminal column count changes).

#[path = "reflow_map.rs"]
mod reflow_map;

use self::reflow_map::{
    ExtrasCopyCtx, ExtrasSource, TrackedPoint, chunk_cells_to_rows, copy_cells_to_row,
    source_coords_for_row,
};
use super::resize_handback::{BaseTop, Edge, Grown, Refill, ResizeLaw, RewrapPlan, Seams, Shrunk};
use super::resize_undo::UndoRow;
use super::row_u16;
use super::scroll_convert::ScrolledRowExtras;
use super::state::DetachedReaderAim;
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

/// Who repaints the viewport after a resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ResizePolicy {
    /// The grid is the frame (Unix PTYs, replays, tests).
    #[default]
    Native,
    /// Conhost repaints the viewport after every resize (above).
    ConPty,
}

#[derive(Clone, Copy)]
enum BottomRows {
    Appended(usize),
    Trimmed(usize),
}

struct ReflowResult {
    rows: Vec<Row>,
    pages: PageStore,
    extras: CellExtras,
    cursor_row: usize,
    cursor_col: u16,
    /// Where the saved (decsc) cursor landed, when it named a rewrapped row.
    saved: Option<(usize, u16)>,
    /// The new row and column the owed window's top landed on ([`BaseTop::Below`]).
    base: Option<(usize, u16)>,
}

#[derive(Clone, Copy, Debug)]
struct Laid {
    /// The blank rows under the cursor the rows end on, which the bottom-anchoring fill leaves.
    tail: usize,
    /// The owed window's top in rows below the screen's top (above it when negative) once the top rows are pushed, before the fill moves it down.
    top: isize,
    /// The window's rows past the height under a cursor too near the top to push more top rows.
    pushed: usize,
    /// The wraps that push took off at its seams (`Grid::finalize_reflow`).
    seams: Seams,
    /// The cells into its row at which the window's top starts.
    skew: u16,
}

impl Laid {
    fn plan(result: &ReflowResult, rows: usize, plan: RewrapPlan) -> Option<(usize, Self)> {
        let blank = |row: &&Row| row.is_empty() && !row.is_wrapped();
        let tail_now = result
            .rows
            .iter()
            .skip(result.cursor_row + 1)
            .rev()
            .take_while(blank)
            .count();
        let content = result.rows.len() - tail_now;
        let (window, top, skew) = match plan.top {
            BaseTop::Above(rows, skew) => (content + rows, -isize::try_from(rows).ok()?, skew),
            BaseTop::Below(..) => {
                let (row, skew) = result.base?;
                (
                    content.saturating_sub(row),
                    isize::try_from(row).ok()?,
                    skew,
                )
            }
        };
        let tail = rows.saturating_sub(window).min(plan.tail);
        let over = (content + tail).saturating_sub(rows);
        let push = over.min(result.cursor_row);
        let top = top - isize::try_from(push).ok()?;
        Some((
            push,
            Self {
                tail,
                top,
                pushed: over - push,
                seams: Seams::default(),
                skew,
            },
        ))
    }
}

/// Where the cursor's next glyph goes, read before a resize moves anything. The resize moves the
/// cursor's own cell; the glyph goes there, or one column on from it under an edge wrap.
#[derive(Clone, Copy, Debug)]
struct NextWrite {
    /// A wrap is pending on the row's last column: the glyph goes on the cell after the one just
    /// printed, which [`Grid::place_next_write`] finds from where that cell landed, so the cursor
    /// stays on its cell's row (Ghostty's `Screen.resize` rule for a saved wrap).
    edge_wrap: bool,
    /// A wrap pending short of the row's last column, at a DECSLRM right margin: it stays
    /// pending on the cursor's cell, as xterm keeps `do_wrap`, so the next glyph starts a row.
    margin_wrap: bool,
}

impl NextWrite {
    fn of(grid: &Grid) -> Self {
        let s = &grid.storage;
        let wrap = s.pending_wrap();
        let at_edge = s.cursor.col >= s.max_col_for_row(s.cursor.row);
        Self {
            edge_wrap: wrap && at_edge,
            margin_wrap: wrap && !at_edge,
        }
    }

    /// The column the glyph lands on, from the column the cursor's cell landed on.
    fn col(self, landed: u16) -> u16 {
        landed.saturating_add(u16::from(self.edge_wrap))
    }

    /// The cursor's column and pending wrap on a row whose last column is `last`, from the column
    /// the cursor's cell landed on. A next write that fits takes its column; one past the row's
    /// end is an edge wrap's (the glyph just printed sits on `last`), which waits there with the
    /// wrap pending, as if the program had printed up to the edge; otherwise the cursor's own cell
    /// fell past the end of a row a resize without reflow cut, and it is clamped onto `last` with
    /// no wrap, as xterm clamps it. A margin wrap stays pending.
    fn place(self, landed: u16, last: u16) -> (u16, bool) {
        let next = self.col(landed);
        let wrap = self.margin_wrap || (next > last && self.edge_wrap);
        (next.min(last), wrap)
    }
}

struct RewrapCursors {
    src: [Option<(usize, u16)>; 3],
    dst: [Option<(usize, u16)>; 3],
}

const LIVE: usize = 0;
const SAVED: usize = 1;
const BASE: usize = 2;

impl RewrapCursors {
    fn cols_on(&self, row: usize) -> [Option<u16>; 3] {
        self.src
            .map(|src| src.and_then(|(r, c)| (r == row).then_some(c)))
    }

    /// Point `i` on a row laid in place at `col`: the live cursor's column stays unclamped, so
    /// [`Grid::place_next_write`] sees a next write past the row's end.
    const fn placed(i: usize, col: u16, new_cols: u16) -> u16 {
        if i == LIVE || col < new_cols {
            col
        } else {
            new_cols.saturating_sub(1)
        }
    }

    fn place_on_row(&mut self, row: usize, new_row: usize, new_cols: u16) {
        for (i, col) in self.cols_on(row).into_iter().enumerate() {
            if let Some(col) = col {
                self.dst[i] = Some((new_row, Self::placed(i, col, new_cols)));
            }
        }
    }

    const fn points(offsets: [Option<usize>; 3]) -> [TrackedPoint; 3] {
        let [live, saved, base] = offsets;
        [
            TrackedPoint {
                offset: live,
                at: None,
                keeps_gap: true,
                opens_row: false,
            },
            TrackedPoint {
                offset: saved,
                at: None,
                keeps_gap: true,
                opens_row: true,
            },
            TrackedPoint {
                offset: base,
                at: None,
                keeps_gap: false,
                opens_row: false,
            },
        ]
    }

    fn land(&mut self, points: &[TrackedPoint; 3]) {
        for (i, point) in points.iter().enumerate() {
            if let Some(at) = point.at {
                self.dst[i] = Some(at);
            }
        }
    }
}

/// May the rewrap merge consume `row` as a soft-wrap continuation of the preceding row?
pub(super) fn is_mergeable_continuation(row: &Row) -> bool {
    row.is_wrapped() && row.line_size() == LineSize::SingleWidth
}

/// How many cells of the cursor's row a rewrap carries: its content and, past it, every cell up to
/// and including the cursor's, so a cursor after the text keeps its place in the line and its
/// cell lands on a row of the line (a new continuation row when the last one is full), as
/// Ghostty's reflow and conhost carry them.
fn cursor_row_copy_len(row: &Row, cursor_col: u16) -> usize {
    usize::from(row.len())
        .max(usize::from(cursor_col) + 1)
        .min(row.as_slice().len())
}

#[allow(clippy::too_many_arguments)]
fn resize_double_width_row_in_place(
    row: &Row,
    row_idx: usize,
    new_cols: u16,
    new_pages: &mut PageStore,
    new_rows: &mut Vec<Row>,
    cursors: &mut RewrapCursors,
    old_extras: Option<&CellExtras>,
    new_extras: &mut CellExtras,
) {
    let content_len = row.len() as usize;
    // SAFETY: `new_row` is appended to `new_rows` and returned alongside `new_pages` in the same reflow result.
    let mut new_row = unsafe { Row::new(new_cols, new_pages) };
    new_row.set_line_size(row.line_size());
    if row.is_wrapped() {
        new_row.set_wrapped(true);
    }
    if content_len > 0 {
        let cells = &row.as_slice()[..content_len];
        let copy_len = content_len.min(new_cols as usize);
        let dest_row = row_u16(new_rows.len());
        let mut extras_ctx = ExtrasCopyCtx {
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
            content_len,
            new_cols,
            dest_row,
            &mut extras_ctx,
        );
    }
    cursors.place_on_row(row_idx, new_rows.len(), new_cols);
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
    // COST: UNBOUNDED(scrollback-width-reflow)
    pub fn resize_with_reflow_mode(
        &mut self,
        new_rows: u16,
        new_cols: u16,
        reflow_mode: ReflowMode,
    ) {
        self.resize_with_reflow_mode_and_policy(
            new_rows,
            new_cols,
            reflow_mode,
            ResizePolicy::Native,
        );
    }

    /// [`Grid::resize`] under an explicit [`ResizePolicy`].
    pub fn resize_with_policy(&mut self, new_rows: u16, new_cols: u16, policy: ResizePolicy) {
        self.resize_with_reflow_mode_and_policy(new_rows, new_cols, ReflowMode::Enabled, policy);
    }

    /// [`Grid::resize_no_reflow`] under an explicit [`ResizePolicy`].
    pub fn resize_no_reflow_with_policy(
        &mut self,
        new_rows: u16,
        new_cols: u16,
        policy: ResizePolicy,
    ) {
        self.resize_with_reflow_mode_and_policy(new_rows, new_cols, ReflowMode::Disabled, policy);
    }

    /// Lend this grid the host's saved cursor (decsc, or the 1049 slot) for the resize.
    pub fn lend_saved_cursor(&mut self, saved: Option<super::Cursor>) {
        self.storage.saved_cursor = crate::SavedCursor {
            cursor: saved.unwrap_or_default(),
            valid: saved.is_some(),
            pending_wrap: false,
        };
    }

    /// Take back the saved cursor lent by [`Self::lend_saved_cursor`], where the resize carried it.
    pub fn take_lent_saved_cursor(&mut self) -> Option<super::Cursor> {
        let saved = std::mem::take(&mut self.storage.saved_cursor);
        saved.valid.then_some(saved.cursor)
    }

    fn resize_with_reflow_mode_and_policy(
        &mut self,
        new_rows: u16,
        new_cols: u16,
        reflow_mode: ReflowMode,
        policy: ResizePolicy,
    ) {
        let handed = self.hand_back_before_resize(new_cols, reflow_mode, policy);
        let rows_step = self.split_rows_step(new_rows, new_cols, reflow_mode, policy);
        self.resize_axes(new_rows, new_cols, reflow_mode, policy);
        if let Some(handed) = handed {
            self.merge_handback_step(handed);
        }
        if let Some(rows_step) = rows_step {
            self.merge_rows_step(rows_step);
        }
    }

    /// The rows half of a resize that moves both the rows and the columns of a reflowing grid.
    pub(super) fn split_rows_step(
        &mut self,
        new_rows: u16,
        new_cols: u16,
        reflow_mode: ReflowMode,
        policy: ResizePolicy,
    ) -> Option<crate::ResizeShape> {
        let new_rows = new_rows.clamp(1, MAX_GRID_ROWS);
        let new_cols = new_cols.clamp(1, MAX_GRID_COLS);
        let rows = self.settled_rows();
        let split = matches!(reflow_mode, ReflowMode::Enabled)
            && policy == ResizePolicy::Native
            && new_cols != self.storage.cols
            && new_rows != rows
            && self.keeps_handback_history()
            && !self.handback_one_pass()
            && match self.storage.resize_law {
                ResizeLaw::Rewrap | ResizeLaw::Undo => false,
                ResizeLaw::HandBack => !(new_rows > rows && new_cols < self.storage.cols),
                ResizeLaw::PushedRows => {
                    new_rows < rows
                        || new_cols > self.storage.cols
                        || self.narrowing_moves_no_row(usize::from(new_rows - rows), new_cols)
                }
            };
        if !split {
            return None;
        }
        let cols = self.storage.cols;
        let offset = std::mem::take(&mut self.storage.display_offset);
        let owed = self.storage.fill_debt.rows;
        self.resize_axes(new_rows, cols, reflow_mode, policy);
        self.storage.display_offset = offset;
        self.storage.last_resize_row_shift = 0;
        let rows = self.storage.last_resize_shape;
        let back = self.demoted_rows_at(rows.pushed, rows.demoted, new_cols);
        self.offer_refill(Refill {
            trimmed: rows.trimmed,
            back,
            owed: row_u16(owed.saturating_sub(self.storage.fill_debt.rows)),
        });
        Some(rows)
    }

    fn narrowing_moves_no_row(&self, reveal: usize, new_cols: u16) -> bool {
        let row_fits = |row: &Row| {
            !row.is_wrapped()
                && (new_cols..row.cols()).all(|c| row.get(c).is_none_or(|cell| cell.is_empty()))
        };
        self.storage.cursor.col < new_cols
            && (0..self.storage.visible_rows)
                .all(|r| self.storage.row_at_screen(r).is_none_or(row_fits))
            && (0..=reveal).all(|i| match self.try_history_line_rev(i) {
                Ok(None) => true,
                Ok(Some(line)) => {
                    !line.is_wrapped()
                        && super::scrollback_reflow::reflow_scrollback_lines(
                            std::slice::from_ref(&*line),
                            new_cols,
                        )
                        .len()
                            <= 1
                }
                Err(_) => false,
            })
    }

    /// One record for a resize [`Self::split_rows_step`] ran in two.
    pub(super) fn merge_rows_step(&mut self, rows: crate::ResizeShape) {
        let width = self.storage.last_resize_shape;
        self.storage.last_resize_shape = crate::ResizeShape {
            reflowed: width.reflowed,
            cut: width.cut,
            undone: false,
            ..rows
        };
    }

    /// One resize pass over both axes at once (see [`Self::resize_with_reflow_mode_and_policy`]).
    pub(super) fn resize_axes(
        &mut self,
        new_rows: u16,
        new_cols: u16,
        reflow_mode: ReflowMode,
        policy: ResizePolicy,
    ) {
        let reflow = matches!(reflow_mode, ReflowMode::Enabled);
        let conpty = policy == ResizePolicy::ConPty;
        let new_rows = new_rows.clamp(1, MAX_GRID_ROWS);
        let new_cols = new_cols.clamp(1, MAX_GRID_COLS);
        let old_cols = self.storage.cols;
        self.admit_resize_undo(new_rows, new_cols, policy);
        let next = NextWrite::of(self);
        let old_rows = self.settled_rows();
        let law = self.storage.resize_law;
        let rewrap_plan = (law == ResizeLaw::Rewrap
            && new_cols != old_cols
            && reflow
            && !conpty
            && !self.handback_one_pass()
            && self.keeps_handback_history())
        .then(|| self.take_plan().or_else(|| self.prepare_rewrap(new_cols)))
        .flatten();
        let refill = self.begin_pass();
        let owed = self.fill_debt_at_pass_start();
        self.storage.last_resize_shape = crate::ResizeShape {
            reflowed: new_cols != old_cols && reflow,
            ..crate::ResizeShape::default()
        };
        let owed_across_rewrap = (law != ResizeLaw::Rewrap
            && new_cols != old_cols
            && reflow
            && !conpty
            && new_rows == old_rows)
            .then(|| self.handback_owed(true));
        if new_cols != old_cols && owed_across_rewrap.is_none() {
            self.drop_handback();
        }

        let prev_offset = self.storage.display_offset;
        let prev_anchor = (prev_offset > 0).then(|| self.top_visible_absolute_row());
        self.storage.display_offset = 0;

        let pre_trailing_blanks = (new_cols != old_cols && reflow && !conpty)
            .then(|| self.trailing_blank_rows_below_cursor());

        let reflowed_scrollback = if new_cols != old_cols && reflow {
            let mut old = self.take_scrollback_lines();
            if conpty {
                self.sever_top_row_continuation();
            } else {
                old.extend(self.take_boundary_continuation_lines());
            }
            if old.first().is_some_and(aterm_scrollback::Line::is_wrapped) {
                let run = self.storage.lazy_buffer.trailing_run();
                old.splice(0..0, self.storage.lazy_buffer.drain_back(run));
            }
            #[cfg(any(test, debug_assertions))]
            super::count_scrollback_reflow_sync_lines(old.len());
            let lines = super::scrollback_reflow::reflow_scrollback_lines(&old, new_cols);
            Some(lines)
        } else {
            None
        };

        let rows_only = new_cols == old_cols;
        if !rows_only {
            self.storage.ring_extras.clear();
            self.drain_lazy_buffer();
        }
        let staged_before_reflow = self.storage.lazy_buffer.len();
        if staged_before_reflow > 0 && new_cols != old_cols && reflow {
            self.storage.staged_width_stale = true;
        }

        if new_cols != old_cols {
            self.storage.lazy_buffer.clear_pool();
        }

        let cursor_row = self.storage.cursor.row as usize;
        let mut landed = self.storage.cursor.col;

        let mut rewrap_pushed = 0;
        let mut laid = None;
        if new_cols != old_cols && reflow {
            (rewrap_pushed, laid, landed) =
                self.reflow_columns(new_rows, new_cols, cursor_row, landed, policy, rewrap_plan);
        } else if new_cols != old_cols {
            // No reflow - just resize each row, after counting what a narrowing cuts off the screen (`ResizeShape::cut`).
            if new_cols < old_cols {
                self.storage.last_resize_shape.cut = self.drawn_columns_past(new_cols);
            }
            let mut new_pages = PageStore::new();
            for row in &mut self.storage.rows {
                // SAFETY: `new_pages` stays alive until it replaces `self.storage.pages` at the end of this branch, so every resized row keeps a live backing store for at least as long as the row remains in `self`.
                unsafe { row.resize(new_cols, &mut new_pages) };
            }
            self.storage.pages = new_pages;
            if new_cols < old_cols {
                self.storage.extras.retain_cols_below(new_cols);
                for row in 0..self.storage.visible_rows {
                    self.drop_unclaimed_extras(row, new_cols - 1..new_cols);
                }
            }
        }

        if !(new_cols != old_cols && reflow) {
            self.migrate_complex_ring_to_extras();
            self.migrate_rgb_ring_to_extras();
        }

        let RowCountChange {
            revealed,
            extras: reveal_extras,
            owed: owed_reveal,
        } = self.adjust_row_count(new_rows, new_cols, policy);
        if revealed > 0 {
            self.storage.history_reveal_gen = self.storage.history_reveal_gen.wrapping_add(1);
            self.note_rows_rekeyed();
        }
        self.storage.extras.retain_rows_below(new_rows);
        self.storage.extras.invalidate_rings();
        self.storage.resize_viewport_state(new_rows, new_cols);
        if revealed > 0 && !(new_cols != old_cols && reflow) {
            self.storage.last_resize_row_shift = row_u16(revealed);
            let bound = new_rows.saturating_sub(1);
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
                self.storage.saved_cursor.cursor.row = self
                    .storage
                    .saved_cursor
                    .cursor
                    .row
                    .saturating_add(row_u16(revealed));
            }
        }
        self.place_next_write(next, landed);
        for (row, bx) in reveal_extras {
            self.inject_scrolled_extras(row, &bx);
        }
        self.paint_wrap_holes(0..row_u16(revealed));
        if rows_only {
            self.settle_fill_debt_rows(owed_reveal);
        }
        if let Some(lines) = reflowed_scrollback {
            self.restore_reflowed_scrollback(lines, new_cols, staged_before_reflow);
        }
        if let (Some(laid), Some(plan)) = (laid, rewrap_plan) {
            // From a plan the fill leaves the window's blank rows that fit and pulls history for the rest; what is owed after is where that leaves the window.
            if laid.pushed > 0 {
                self.drop_fill_debt();
                self.storage.last_resize_shape.pushed = row_u16(laid.pushed);
                self.set_handback(
                    Edge::after_rewrap(laid.top, 0, plan.tail, laid.pushed).with_seams(laid.seams),
                );
                self.keep_skew(laid.top, laid.skew);
            } else {
                let pulled = self.fill_viewport_deficit_from_history(laid.tail);
                let deferred = self.owe_fill_after_width_pass(laid.tail);
                let top = laid
                    .top
                    .saturating_add(isize::try_from(pulled).unwrap_or(isize::MAX));
                self.owe_after_rewrap(top, laid.skew, laid.tail, plan.tail, deferred);
            }
        } else {
            let refilled = pre_trailing_blanks.map_or(0, |target| {
                refill.blanks_back(
                    self.trailing_blank_rows_below_cursor()
                        .saturating_sub(target),
                )
            });
            if let Some(pre) = pre_trailing_blanks {
                self.fill_after_width_pass(owed, pre, refilled, refill.owed);
            }
            if let Some(owed) = owed_across_rewrap {
                self.keep_handback_across_rewrap(owed, rewrap_pushed + refilled);
            }
        }
        let shape = self.storage.last_resize_shape;
        if shape.pushed > 0 || shape.restored_bottom > 0 {
            self.note_push_gap();
        }
        match prev_anchor {
            Some(anchor) if new_cols == old_cols => self.scroll_to_absolute_row(anchor),
            _ => self.storage.display_offset = prev_offset.min(self.storage.scrollback_lines()),
        }
        self.storage.pages.shrink_to_fit();
        self.storage.damage = Damage::Full;
        self.storage.content_gen += 1;
        if new_cols != old_cols && reflow {
            self.storage.history_renumber_epoch =
                self.storage.history_renumber_epoch.saturating_add(1);
            self.lose_anchors_renumbered();
        }
        self.cover_retained_rows();
        self.seal_resize_undo();
    }

    /// Put the cursor where the resize took its next glyph, from the column its cell `landed` on
    /// ([`NextWrite::place`]). A rewrap carries the cursor's cell into its line, so only an edge
    /// wrap's next write, or a cell a resize without reflow cut off, falls past the row's end.
    fn place_next_write(&mut self, next: NextWrite, landed: u16) {
        let row = self.storage.cursor.row;
        let (col, wrap) = next.place(landed, self.storage.max_col_for_row(row));
        self.storage.set_cursor_position(row, col);
        if wrap {
            self.storage.mark_pending_wrap();
        } else {
            self.storage.clear_pending_wrap();
        }
    }

    fn sever_top_row_continuation(&mut self) {
        let len = self.storage.rows.len();
        if len == 0 || self.storage.visible_rows == 0 {
            return;
        }
        let hist = self
            .storage
            .total_lines
            .saturating_sub(usize::from(self.storage.visible_rows));
        let top = (self.storage.ring_head + hist) % len;
        if let Some(row) = self.storage.rows.get_mut(top) {
            row.set_wrapped(false);
        }
    }

    fn drawn_columns_past(&self, new_cols: u16) -> u16 {
        let mut cut = 0u16;
        for r in 0..self.storage.visible_rows {
            let Some(row) = self.storage.row_at_screen(r) else {
                continue;
            };
            if let Some(col) = (new_cols.saturating_add(cut)..row.cols())
                .rev()
                .find(|&c| row.get(c).is_some_and(|cell| !cell.is_empty()))
            {
                cut = col + 1 - new_cols;
            }
        }
        cut
    }

    /// Copy every visible complex cell's ring-stored codepoint into the persistent HashMap extras.
    pub(super) fn migrate_complex_ring_to_extras(&mut self) {
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

    /// Copy every visible cell's ring-stored 24-bit fg/bg into the persistent extras map.
    pub(super) fn migrate_rgb_ring_to_extras(&mut self) {
        if !self.storage.extras.has_rgb_ring() {
            return;
        }
        let rows = usize::from(self.storage.visible_rows).min(self.storage.rows.len());
        let cols = self.storage.cols;
        let mut flagged: Vec<(u16, bool, bool)> = Vec::new();
        for r in 0..rows {
            let row = row_u16(r);
            flagged.clear();
            if let Some(rw) = self.row(row) {
                for col in 0..cols {
                    if let Some(cell) = rw.get(col) {
                        let (fg, bg) = (cell.fg_needs_overflow(), cell.bg_needs_overflow());
                        if fg || bg {
                            flagged.push((col, fg, bg));
                        }
                    }
                }
            }
            #[cfg(test)]
            crate::test_counters::count_rgb_harvest_cells(usize::from(cols));
            for &(col, fg_flag, bg_flag) in &flagged {
                let fg = fg_flag
                    .then(|| self.storage.extras.fg_rgb_for(row, col))
                    .flatten();
                let bg = bg_flag
                    .then(|| self.storage.extras.bg_rgb_for(row, col))
                    .flatten();
                if fg.is_none() && bg.is_none() {
                    continue;
                }
                let extra = self.storage.extras.get_or_create(CellCoord::new(row, col));
                if let Some(rgb) = fg {
                    extra.set_fg_rgb(Some(rgb));
                }
                if let Some(rgb) = bg {
                    extra.set_bg_rgb(Some(rgb));
                }
            }
        }
    }

    fn adjust_row_count(
        &mut self,
        target_rows: u16,
        new_cols: u16,
        policy: ResizePolicy,
    ) -> RowCountChange {
        let target = target_rows as usize;
        let old_visible = usize::from(self.storage.visible_rows);

        if new_cols == self.storage.cols {
            let native = policy == ResizePolicy::Native;
            let (top, skew) = (self.handback_owed(native).top(), self.window_skew());
            let before = self.storage.last_resize_shape;
            let moved = self.adjust_row_count_rows_only(target, new_cols, policy);
            if native && skew > 0 {
                let after = self.storage.last_resize_shape;
                let shift = |now: u16, was: u16| {
                    isize::from(now.cast_signed()) - isize::from(was.cast_signed())
                };
                let top = top - shift(after.demoted, before.demoted)
                    + shift(after.revealed, before.revealed);
                self.keep_skew(top, skew);
            }
            return moved;
        }

        if self.storage.rows.len() > target {
            #[cfg(any(test, debug_assertions))]
            if new_cols == self.storage.cols {
                super::count_rows_only_resize_migrated_rows(self.storage.rows.len() - target);
            }
            self.storage.linearize_ring();
            let excess = self.storage.rows.len() - target;
            let scrollback = self
                .storage
                .total_lines
                .saturating_sub(self.storage.visible_rows as usize);
            let from_front = excess.min(scrollback);
            let from_back = excess - from_front;
            self.storage.last_resize_shape.pushed = row_u16(from_back);
            if from_front > 0 {
                // Stage front rows when a retaining store is attached (without one `drain_lazy_buffer` would discard them) or detached for an off-thread reflow (flushed on re-attach).
                if self.storage.stages_evicted_rows() {
                    let drained_rows: Vec<Row> = self.storage.rows.drain(..from_front).collect();
                    for row in drained_rows {
                        let extracted =
                            Self::extract_row_extras(&row, &self.storage.extras, u16::MAX);
                        self.storage.lazy_buffer.push_row(&row, extracted);
                        // SAFETY: these rows came from storage.rows, and staging copied their content before returning their allocations.
                        unsafe { row.recycle(&mut self.storage.pages) };
                    }
                } else {
                    self.storage.recycle_rows(0..from_front);
                }
            }
            if from_back > 0 {
                // Push bottom visible rows to lazy scrollback before discarding, so a height shrink does not lose them, wherever a store keeps them or a detached reflow will flush them (as above).
                if self.storage.stages_evicted_rows() {
                    let start = self.storage.rows.len() - from_back;
                    let remaining_scrollback = scrollback.saturating_sub(from_front);
                    let drained_rows: Vec<Row> = self.storage.rows.drain(start..).collect();
                    for (i, row) in drained_rows.into_iter().enumerate() {
                        let external_row = row_u16(start + i - remaining_scrollback);
                        let extracted =
                            Self::extract_row_extras(&row, &self.storage.extras, external_row);
                        self.storage.lazy_buffer.push_row(&row, extracted);
                        // SAFETY: the removed row belongs to storage.pages; the lazy buffer owns its content before this transfer.
                        unsafe { row.recycle(&mut self.storage.pages) };
                    }
                } else {
                    let end = self.storage.rows.len();
                    self.storage.recycle_rows(end - from_back..end);
                }
            }
        }

        self.storage.total_lines = self.storage.rows.len();

        let revealed = target
            .saturating_sub(old_visible)
            .min(self.storage.rows.len().saturating_sub(old_visible));
        self.storage.last_resize_shape.revealed = row_u16(revealed);

        if target > self.storage.rows.len() {
            self.storage.linearize_ring();
            let rows_to_add = target - self.storage.rows.len();
            {
                let rows = &mut self.storage.rows;
                let pages = &mut self.storage.pages;
                // SAFETY: New rows are stored in the same `GridStorage` that owns `pages`, and rows drop before the backing pages.
                for _ in 0..rows_to_add {
                    rows.push(unsafe { Row::new(new_cols, pages) });
                }
            }
            self.storage.total_lines += rows_to_add;
            self.storage.last_resize_shape.appended = row_u16(rows_to_add);
        }
        RowCountChange::revealed(revealed, Vec::new())
    }

    fn adjust_row_count_rows_only(
        &mut self,
        target: usize,
        new_cols: u16,
        policy: ResizePolicy,
    ) -> RowCountChange {
        let visible = self.storage.visible_rows as usize;
        debug_assert_eq!(
            self.storage.total_lines,
            self.storage.rows.len(),
            "rows-only resize: every ring row is a retained line"
        );
        let owed = self.handback_owed(policy == ResizePolicy::Native);
        let mut next = owed;
        self.seat_pushed_rows_in_ring(owed);

        if target < visible {
            let shrink = visible - target;
            self.storage.linearize_ring();
            let cursor_row = usize::from(self.storage.cursor.row).min(visible - 1);

            let plan = owed.plan_shrink(shrink, cursor_row);

            let blank = self.trailing_blank_rows_below_cursor();
            let pushes_owed = self.storage.resize_law >= ResizeLaw::Rewrap
                && owed.pushed_rows() > 0
                && cursor_row == 0
                && self.storage.total_lines - visible >= usize::from(owed.pushed_rows())
                && blank <= usize::from(owed.seams().gap);
            let trim = if pushes_owed {
                0
            } else {
                (shrink - plan.unreveal).min(blank)
            };
            self.storage.last_resize_shape.trimmed = row_u16(trim);
            let counted = self.storage.resize_undo.is_none() && policy == ResizePolicy::Native;
            if let Some(undo) = self.storage.resize_undo.as_mut() {
                undo.entries
                    .extend(std::iter::repeat_with(|| UndoRow::Trimmed).take(trim));
            }
            if trim > 0 {
                let keep = self.storage.total_lines - trim;
                self.storage.recycle_rows(keep..self.storage.rows.len());
                self.storage.total_lines = keep;
                self.keep_keys_across_bottom_rows(BottomRows::Trimmed(trim));
            }
            let remaining = shrink - trim;

            let demote = remaining.min(cursor_row);
            debug_assert!(
                demote >= plan.unreveal,
                "the shrink demotes the revealed lines"
            );
            let hist = self.storage.total_lines - (visible - trim);
            if demote > 0 {
                while self.storage.ring_extras.len() < hist {
                    self.storage.ring_extras.push_back(None);
                }
                for i in 0..demote {
                    let extracted = Self::extract_row_extras(
                        &self.storage.rows[hist + i],
                        &self.storage.extras,
                        row_u16(i),
                    );
                    self.storage.push_ring_extras(extracted);
                }
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
            let older_pushed = usize::from(owed.pushed_rows());
            let above_older = older_pushed > 0 && trim == 0 && demote == 0 && hist >= older_pushed;
            next = if older_pushed > 0 && !above_older {
                Edge::None
            } else {
                owed
            }
            .after_shrink(&Shrunk {
                rows: shrink,
                plan,
                trimmed: trim,
                pushed: bottom_push,
                counted,
            });
            if self.storage.resize_law == ResizeLaw::HandBack && bottom_push > 0 {
                next = Edge::None;
            }
            self.storage.last_resize_shape.demoted = row_u16(demote);
            self.storage.last_resize_shape.pushed = row_u16(bottom_push);
            self.storage.last_resize_row_shift = row_u16(demote);
            if bottom_push > 0 {
                self.force_selection_invalidation();
            } else if demote > 0 {
                self.invalidate_host_coordinates_keeping_rows();
            }

            if bottom_push > 0 {
                let hist_after_demote = hist + demote;
                let surviving = visible - trim - demote;
                while self.storage.ring_extras.len() < hist_after_demote {
                    self.storage.ring_extras.push_back(None);
                }
                for i in target..surviving {
                    let extracted = Self::extract_row_extras(
                        &self.storage.rows[hist_after_demote + i],
                        &self.storage.extras,
                        row_u16(i),
                    );
                    self.storage.push_ring_extras(extracted);
                }
                self.storage.rows[hist_after_demote..].rotate_left(target);
                let stacked = above_older && next.pushed_rows() > 0;
                if stacked {
                    self.storage.rows[hist - older_pushed..hist + bottom_push]
                        .rotate_right(bottom_push);
                    self.put_pushed_extras_above(older_pushed, bottom_push);
                    self.note_bottom_end_renumbered();
                }
                if self.storage.resize_law >= ResizeLaw::Rewrap {
                    let block = if stacked {
                        hist - older_pushed
                    } else {
                        hist_after_demote
                    };
                    let seams = self.cut_push_seams(
                        block,
                        bottom_push,
                        stacked.then(|| (owed.pushed_rows(), owed.seams())),
                    );
                    next = next.with_seams(seams);
                }
            }

            let excess =
                (self.storage.total_lines - target).saturating_sub(self.storage.max_scrollback);
            if excess > 0 {
                #[cfg(any(test, debug_assertions))]
                super::count_rows_only_resize_migrated_rows(excess);
                if self.storage.stages_evicted_rows() {
                    for i in 0..excess {
                        let extras = self.storage.ring_extras.pop_front().flatten();
                        let storage = &mut self.storage;
                        storage.lazy_buffer.push_row_boxed(&storage.rows[i], extras);
                    }
                } else if self.storage.resize_undo.is_some() && excess == demote + bottom_push {
                    let storage = &mut self.storage;
                    let mut taken: Vec<_> = (0..excess)
                        .map(|i| {
                            let extras = storage.ring_extras.pop_front().flatten();
                            (storage.rows[i].snapshot(), extras)
                        })
                        .collect();
                    let pushed = taken.split_off(demote);
                    if let Some(undo) = storage.resize_undo.as_mut() {
                        undo.entries.extend(
                            taken
                                .into_iter()
                                .map(|(row, extras)| UndoRow::Demoted(row, extras)),
                        );
                        undo.entries.extend(
                            pushed
                                .into_iter()
                                .rev()
                                .map(|(row, extras)| UndoRow::Pushed(row, extras)),
                        );
                    }
                    storage.last_resize_shape.stashed = row_u16(excess);
                } else {
                    self.storage.resize_undo = None;
                    for _ in 0..excess {
                        self.storage.ring_extras.pop_front();
                    }
                }
                self.storage.recycle_rows(0..excess);
                self.storage.total_lines -= excess;
            }
            if usize::from(next.pushed_rows()) > self.storage.total_lines - target
                && !(self.storage.resize_law >= ResizeLaw::Rewrap
                    && self.storage.stages_evicted_rows())
            {
                next = Edge::None;
            }
        } else if target > visible {
            if policy == ResizePolicy::ConPty {
                self.storage.linearize_ring();
                let rows_to_add = target - visible;
                let rows = &mut self.storage.rows;
                let pages = &mut self.storage.pages;
                for _ in 0..rows_to_add {
                    // SAFETY: New rows are stored in the same `GridStorage` that owns `pages`, and rows drop before the backing pages.
                    rows.push(unsafe { Row::new(new_cols, pages) });
                }
                self.storage.total_lines += rows_to_add;
                self.storage.last_resize_shape.appended = row_u16(rows_to_add);
                self.keep_keys_across_bottom_rows(BottomRows::Appended(rows_to_add));
                self.set_handback(Edge::None);
                return RowCountChange::revealed(0, Vec::new());
            }
            let grow = target - visible;
            let restored_bottom = self.take_undo_rows(grow, new_cols);
            let after_undo = grow - restored_bottom.len();
            let ring = self.storage.total_lines - visible;
            let pushed = usize::from(owed.pushed_rows());
            let owed = if pushed > ring || (pushed > 0 && !restored_bottom.is_empty()) {
                Edge::None
            } else {
                owed
            };
            let pushed = usize::from(owed.pushed_rows());
            let (unpushed, natural, handed_back) = owed.plan_grow(after_undo);
            let want = after_undo - unpushed - handed_back;
            let ring = ring - unpushed;
            if want > ring {
                self.pull_history_into_ring(want - ring);
            }
            let mut unpushed_extras = Vec::new();
            let mut seams = owed.seams();
            if unpushed > 0 {
                self.storage.linearize_ring();
                let hist = self.storage.total_lines - visible;
                self.storage.rows[hist - pushed..hist + visible].rotate_left(unpushed);
                if self.storage.resize_law >= ResizeLaw::Rewrap {
                    seams = self.mend_push_seams(hist, visible, pushed, unpushed, seams);
                }
                unpushed_extras = self.take_unpushed_extras(pushed, unpushed);
                self.storage.last_resize_shape.restored_bottom = row_u16(unpushed);
                self.force_selection_invalidation();
                self.note_bottom_end_renumbered();
                self.storage.history_reveal_gen = self.storage.history_reveal_gen.wrapping_add(1);
                self.note_rows_rekeyed();
            }
            let hist = self.storage.total_lines - visible - unpushed;
            let revealed = want.min(hist);
            let owed_reveal = if self.storage.scrollback_detached_for_reflow {
                want - revealed
            } else {
                0
            };
            self.storage.last_resize_shape.revealed = row_u16(revealed);
            let keep = hist - revealed;
            let mut reveal_extras: Vec<(u16, Box<ScrolledRowExtras>)> = Vec::new();
            for j in (0..revealed).rev() {
                if self.storage.ring_extras.len() <= keep {
                    break;
                }
                if let Some(bx) = self.storage.ring_extras.pop_back().flatten() {
                    reveal_extras.push((row_u16(j), bx));
                }
            }
            for (i, bx) in unpushed_extras {
                reveal_extras.push((row_u16(revealed + visible + i), bx));
            }
            let mut blanks_restored = 0;
            let restored_bottom_count = restored_bottom.len();
            if !restored_bottom.is_empty() {
                self.storage.linearize_ring();
                let base = revealed + visible;
                let n = restored_bottom.len();
                let mut pushed = 0;
                let rows = &mut self.storage.rows;
                let pages = &mut self.storage.pages;
                for (i, entry) in restored_bottom.into_iter().enumerate() {
                    let Some((row, extras)) = entry else {
                        // SAFETY: New rows are stored in the same `GridStorage` that owns `pages`, and rows drop before the backing pages.
                        rows.push(unsafe { Row::new(new_cols, pages) });
                        blanks_restored += 1;
                        continue;
                    };
                    // SAFETY: the row is stored in the same `GridStorage` that owns `pages`, and rows drop before the backing pages.
                    rows.push(unsafe { Row::from_snapshot(&row, new_cols, pages) });
                    pushed += 1;
                    if let Some(bx) = extras {
                        reveal_extras.push((row_u16(base + i), bx));
                    }
                }
                self.storage.total_lines += n;
                self.storage.last_resize_shape.restored_bottom = row_u16(pushed);
                self.storage.last_resize_shape.appended = row_u16(blanks_restored);
                self.keep_keys_across_bottom_rows(BottomRows::Appended(blanks_restored));
                if pushed > 0 {
                    self.note_bottom_end_renumbered();
                }
            }
            let rows_to_add = grow - revealed - restored_bottom_count - unpushed;
            if rows_to_add > 0 {
                self.storage.linearize_ring();
                let rows = &mut self.storage.rows;
                let pages = &mut self.storage.pages;
                for _ in 0..rows_to_add {
                    // SAFETY: New rows are stored in the same `GridStorage` that owns `pages`, and rows drop before the backing pages.
                    rows.push(unsafe { Row::new(new_cols, pages) });
                }
                self.storage.total_lines += rows_to_add;
                self.storage.last_resize_shape.appended = row_u16(blanks_restored + rows_to_add);
                self.keep_keys_across_bottom_rows(BottomRows::Appended(rows_to_add));
            }
            self.set_handback(
                owed.after_grow(&Grown {
                    rows: after_undo,
                    unpushed,
                    natural,
                    handed_back,
                    revealed,
                    restored_top: usize::from(self.storage.last_resize_shape.restored_top),
                })
                .with_seams(seams),
            );
            return RowCountChange {
                revealed,
                extras: reveal_extras,
                owed: owed_reveal,
            };
        }
        self.set_handback(next);
        RowCountChange::revealed(0, Vec::new())
    }

    fn note_bottom_end_renumbered(&mut self) {
        self.storage.history_renumber_epoch = self.storage.history_renumber_epoch.saturating_add(1);
        self.lose_anchors_renumbered();
    }

    fn keep_keys_across_bottom_rows(&mut self, rows: BottomRows) {
        let moved = |counter: u64| match rows {
            BottomRows::Appended(n) => counter.saturating_add(n as u64),
            BottomRows::Trimmed(n) => counter.saturating_sub(n as u64),
        };
        self.storage.absolute_row_counter = moved(self.storage.absolute_row_counter);
        if let Some(
            DetachedReaderAim::Above { at_counter, .. } | DetachedReaderAim::At { at_counter, .. },
        ) = self.storage.detached_reader_aim.as_mut()
        {
            *at_counter = moved(*at_counter);
        }
    }

    /// The inverse of [`Self::extract_row_extras`].
    pub(super) fn inject_scrolled_extras(&mut self, row_idx: u16, e: &ScrolledRowExtras) {
        for span in &e.hyperlinks {
            for col in span.start_col..span.end_col {
                let cell = self.storage.cell_extra_mut(row_idx, col);
                cell.set_hyperlink(Some(span.url.clone()));
                cell.set_hyperlink_id(span.id.clone());
            }
        }
        for (col, s) in &e.complex_chars {
            self.storage
                .cell_extra_mut(row_idx, *col)
                .set_complex_char(Some(s.clone()));
        }
        for (col, marks) in &e.combining {
            let cell = self.storage.cell_extra_mut(row_idx, *col);
            for c in marks {
                cell.add_combining(*c);
            }
        }
        for (col, packed) in &e.underline_colors {
            self.storage
                .cell_extra_mut(row_idx, *col)
                .set_underline_color_u32(Some(*packed));
        }
        let extras = &mut self.storage.extras;
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
        let cols = self.storage.cols;
        let mut budget = usize::from(cols) * crate::extra::MAX_IMAGE_LAYERS;
        'images: for span in &e.images {
            for col in span.start_col..span.end_col.min(cols) {
                if budget == 0 {
                    break 'images;
                }
                if let Some(layer) = super::scroll_materialize::span_layer_at(span, col) {
                    let _ = self.storage.cell_extra_mut(row_idx, col);
                    self.storage
                        .extras
                        .stack_image(CellCoord::new(row_idx, col), layer);
                }
                budget -= 1;
            }
        }
        let spacers: Vec<u16> = self
            .storage
            .row_at_screen(row_idx)
            .map(|row| {
                let cells = row.as_slice();
                (1..cells.len())
                    .filter(|&i| super::scroll_convert::is_spacer(cells, i))
                    .filter_map(|i| u16::try_from(i).ok())
                    .collect()
            })
            .unwrap_or_default();
        for spacer in spacers {
            let lead = CellCoord::new(row_idx, spacer - 1);
            let Some(values) = self
                .storage
                .extras
                .get(lead)
                .and_then(super::scroll_materialize::SpacerValues::of)
            else {
                continue;
            };
            values.fill(self.storage.cell_extra_mut(row_idx, spacer));
        }
    }

    /// Rewrap the screen to `new_cols`, tracking the live cursor from `cursor_col`: the rows
    /// pushed, the laid plan, and the cursor's column before it is clamped to the width.
    fn reflow_columns(
        &mut self,
        target_rows: u16,
        new_cols: u16,
        cursor_row: usize,
        cursor_col: u16,
        policy: ResizePolicy,
        plan: Option<RewrapPlan>,
    ) -> (usize, Option<Laid>, u16) {
        let old_extras = self
            .storage
            .extras
            .has_any_data()
            .then(|| std::mem::take(&mut self.storage.extras));
        let old_extras_ref = old_extras.as_ref();
        self.reflow_rewrap_columns(
            target_rows,
            new_cols,
            cursor_row,
            cursor_col,
            old_extras_ref,
            policy,
            plan,
        )
    }

    fn finalize_reflow(
        &mut self,
        target_rows: u16,
        mut result: ReflowResult,
        new_cols: u16,
        policy: ResizePolicy,
        plan: Option<RewrapPlan>,
    ) -> (usize, Option<Laid>, u16) {
        let target_rows = usize::from(target_rows);

        let laid = plan.and_then(|plan| Laid::plan(&result, target_rows, plan));
        let push_count = if let Some((push, _)) = laid {
            push
        } else if policy == ResizePolicy::ConPty {
            Self::conpty_reflow_demote(&result, target_rows)
        } else if result.rows.len() > target_rows && result.cursor_row >= target_rows {
            let rows_to_push = result.rows.len() - target_rows;
            rows_to_push.min(result.cursor_row + 1 - target_rows)
        } else {
            0
        };
        if push_count > 0 {
            let drained_rows: Vec<Row> = result.rows.drain(..push_count).collect();
            for (i, row) in drained_rows.into_iter().enumerate() {
                let row_idx = u16::try_from(i).unwrap_or(u16::MAX);
                let extracted = Self::extract_row_extras(&row, &result.extras, row_idx);
                self.storage.lazy_buffer.push_row(&row, extracted);
                // SAFETY: reflow built these rows in result.pages, not the old storage.pages. The lazy buffer now owns the preserved data.
                unsafe { row.recycle(&mut result.pages) };
            }

            if let Ok(n) = u16::try_from(push_count) {
                result.extras.shift_rows_up_by(0, n);
            }
            result.cursor_row -= push_count;
            result.saved = result
                .saved
                .map(|(row, col)| (row.saturating_sub(push_count), col));
        }

        let pushed = laid.map_or(0, |(_, laid)| laid.pushed);
        let mut seams = Seams::default();
        if pushed > 0 {
            let end = (target_rows + pushed).min(result.rows.len());
            let mut bottom: Vec<Row> = result.rows.drain(target_rows..end).collect();
            if let Some(first) = bottom.first_mut().filter(|row| row.is_wrapped()) {
                first.set_wrapped(false);
                seams.bottom = true;
            }
            if let Some(top) = result.rows.first_mut().filter(|row| row.is_wrapped()) {
                top.set_wrapped(false);
                seams.top = true;
            }
            for (i, row) in bottom.into_iter().enumerate() {
                let extracted =
                    Self::extract_row_extras(&row, &result.extras, row_u16(target_rows + i));
                self.storage.lazy_buffer.push_row(&row, extracted);
                // SAFETY: Reflow built these rows in result.pages; the lazy buffer now owns the preserved data.
                unsafe { row.recycle(&mut result.pages) };
            }
        }

        let kept = target_rows.min(result.rows.len());
        for row in result.rows.drain(kept..) {
            // SAFETY: Each removed reflow row belongs to the new result arena.
            unsafe { row.recycle(&mut result.pages) };
        }

        drop(std::mem::take(&mut self.storage.rows));
        self.storage.pages = result.pages;

        // SAFETY: Each padding row is created against `self.storage.pages`, which remains owned by `self` for the lifetime of the inserted rows.
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

        self.storage.any_double_width = self.storage.rows.iter().any(|r| {
            matches!(
                r.line_size(),
                LineSize::DoubleWidth | LineSize::DoubleHeightTop | LineSize::DoubleHeightBottom
            )
        });

        let max_row = row_u16(self.storage.rows.len().saturating_sub(1));
        self.storage.cursor.row = row_u16(result.cursor_row).min(max_row);
        self.storage.cursor.col = result.cursor_col.min(new_cols.saturating_sub(1));
        if let Some((row, col)) = result.saved {
            self.storage.saved_cursor.cursor.row = row_u16(row);
            self.storage.saved_cursor.cursor.col = col.min(new_cols.saturating_sub(1));
        }
        (
            push_count,
            laid.map(|(_, laid)| Laid { seams, ..laid }),
            result.cursor_col,
        )
    }

    fn conpty_reflow_demote(result: &ReflowResult, target_rows: usize) -> usize {
        let content_rows = result
            .rows
            .iter()
            .rposition(|row| !row.is_empty() || row.is_wrapped())
            .map_or(0, |last| last + 1)
            .max(result.cursor_row + 1);
        content_rows
            .saturating_sub(target_rows)
            .min(result.cursor_row)
    }

    #[allow(clippy::too_many_arguments)]
    fn reflow_rewrap_columns(
        &mut self,
        target_rows: u16,
        new_cols: u16,
        cursor_row: usize,
        cursor_col: u16,
        old_extras: Option<&CellExtras>,
        policy: ResizePolicy,
        plan: Option<RewrapPlan>,
    ) -> (usize, Option<Laid>, u16) {
        let mut new_pages = PageStore::new();
        let visible_count = usize::from(self.storage.visible_rows);
        let mut new_rows: Vec<Row> = Vec::with_capacity(visible_count);
        let saved = self.storage.saved_cursor;
        let saved_src = (saved.valid && usize::from(saved.cursor.row) < visible_count)
            .then(|| (usize::from(saved.cursor.row), saved.cursor.col));
        let mut cursors = RewrapCursors {
            src: [
                Some((cursor_row, cursor_col)),
                saved_src,
                plan.and_then(|plan| match plan.top {
                    BaseTop::Below(row, skew) => Some((row, skew)),
                    BaseTop::Above(..) => None,
                }),
            ],
            dst: [None; 3],
        };
        let mut merge_buf: Vec<super::Cell> = Vec::with_capacity(self.storage.cols as usize);
        let mut merge_coords: Vec<CellCoord> = Vec::new();
        let mut new_extras = CellExtras::new();

        let mut i = 0;
        while i < visible_count {
            #[cfg(any(test, debug_assertions))]
            super::count_reflow_row_op();

            let row = match self.row(row_u16(i)) {
                Some(r) => r,
                None => {
                    i += 1;
                    continue;
                }
            };
            let first_row_idx = i;
            let copy_len = if cursor_row == first_row_idx {
                cursor_row_copy_len(row, cursor_col)
            } else {
                row.len() as usize
            };
            let has_cont = i + 1 < visible_count
                && self
                    .row(row_u16(i + 1))
                    .is_some_and(is_mergeable_continuation);

            let source_line_size = row.line_size();

            if source_line_size != LineSize::SingleWidth {
                resize_double_width_row_in_place(
                    row,
                    first_row_idx,
                    new_cols,
                    &mut new_pages,
                    &mut new_rows,
                    &mut cursors,
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
                    &mut cursors,
                    old_extras,
                    &mut new_extras,
                );
            } else if copy_len == 0 {
                // SAFETY: `new_row` is appended to `new_rows` and returned alongside `new_pages` in the same reflow result.
                let mut new_row = unsafe { Row::new(new_cols, &mut new_pages) };
                if row.is_wrapped() {
                    new_row.set_wrapped(true);
                }
                new_row.set_line_size(source_line_size);
                cursors.place_on_row(first_row_idx, new_rows.len(), new_cols);
                new_rows.push(new_row);
            } else {
                let was_wrapped = row.is_wrapped();
                let first_idx = new_rows.len();
                let cells = &row.as_slice()[..copy_len];
                let mut points = RewrapCursors::points(
                    cursors
                        .cols_on(first_row_idx)
                        .map(|col| col.map(usize::from)),
                );
                let mut extras_ctx = ExtrasCopyCtx {
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
                    usize::from(row.len()),
                    new_cols,
                    &mut new_pages,
                    &mut new_rows,
                    &mut points,
                    &mut extras_ctx,
                );
                cursors.land(&points);
                if first_idx < new_rows.len() {
                    if was_wrapped {
                        new_rows[first_idx].set_wrapped(true);
                    }
                    new_rows[first_idx].set_line_size(source_line_size);
                }
            }
            i += 1;
        }
        let (cursor_row, cursor_col) = cursors.dst[LIVE].unwrap_or((cursor_row, cursor_col));
        self.finalize_reflow(
            target_rows,
            ReflowResult {
                rows: new_rows,
                pages: new_pages,
                extras: new_extras,
                cursor_row,
                cursor_col,
                saved: cursors.dst[SAVED],
                base: cursors.dst[BASE],
            },
            new_cols,
            policy,
            plan,
        )
    }

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
        cursors: &mut RewrapCursors,
        old_extras: Option<&CellExtras>,
        new_extras: &mut CellExtras,
    ) {
        merge_buf.clear();
        merge_coords.clear();
        let mut offsets: [Option<usize>; 3] = [None; 3];
        let mut content_end = 0usize;

        let first_row_was_wrapped = self.row(row_u16(start)).is_some_and(Row::is_wrapped);
        let first_row_line_size = self
            .row(row_u16(start))
            .map_or(LineSize::SingleWidth, Row::line_size);

        let mut row_start = merge_buf.len();
        let mut row_idx = start;
        if let Some(row) = self.row(row_u16(start)) {
            let len = row.len() as usize;
            merge_buf.extend_from_slice(&row.as_slice()[..len]);
            if old_extras.is_some() {
                merge_coords.extend(source_coords_for_row(row_u16(start), len));
            }
            if len > 0 {
                content_end = merge_buf.len();
            }
        }
        for (k, col) in cursors.cols_on(start).into_iter().enumerate() {
            if let Some(col) = col {
                offsets[k] = Some(usize::from(col));
            }
        }

        while *i + 1 < visible_count
            && self
                .row(row_u16(*i + 1))
                .is_some_and(is_mergeable_continuation)
        {
            *i += 1;
            #[cfg(any(test, debug_assertions))]
            super::count_reflow_row_op();
            let continued = self
                .row(row_u16(row_idx))
                .map_or(0, |prev| usize::from(prev.continued_len()));
            let row_end = row_start + continued;
            if merge_buf.len() > row_end {
                merge_buf.truncate(row_end);
                if old_extras.is_some() {
                    merge_coords.truncate(row_end);
                }
                content_end = content_end.min(row_end);
            }
            while merge_buf.len() < row_end {
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
                if len > 0 {
                    content_end = merge_buf.len();
                }
                for (k, col) in cursors.cols_on(*i).into_iter().enumerate() {
                    if let Some(col) = col {
                        offsets[k] = Some(off + usize::from(col));
                    }
                }
            }
        }

        if cursor_row == row_idx
            && let Some(row) = self.row(row_u16(row_idx))
        {
            let copied = merge_buf.len() - row_start;
            let end = cursor_row_copy_len(row, cursor_col);
            if end > copied {
                merge_buf.extend_from_slice(&row.as_slice()[copied..end]);
                if old_extras.is_some() {
                    merge_coords.extend(
                        (copied..end).map(|col| CellCoord::new(row_u16(row_idx), row_u16(col))),
                    );
                }
            }
        }

        if merge_buf.is_empty() {
            // SAFETY: `new_row` is appended to `new_rows` and returned alongside `new_pages` in the same reflow result.
            let mut new_row = unsafe { Row::new(new_cols, new_pages) };
            new_row.set_line_size(first_row_line_size);
            for (k, offset) in offsets.into_iter().enumerate() {
                if offset.is_some()
                    && let Some((_, col)) = cursors.src[k]
                {
                    cursors.dst[k] =
                        Some((new_rows.len(), RewrapCursors::placed(k, col, new_cols)));
                }
            }
            new_rows.push(new_row);
        } else {
            let first_idx = new_rows.len();
            let mut points = RewrapCursors::points(offsets);
            let mut extras_ctx = ExtrasCopyCtx {
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
                content_end,
                new_cols,
                new_pages,
                new_rows,
                &mut points,
                &mut extras_ctx,
            );
            cursors.land(&points);
            if first_idx < new_rows.len() {
                if first_row_was_wrapped {
                    new_rows[first_idx].set_wrapped(true);
                }
                new_rows[first_idx].set_line_size(first_row_line_size);
            }
        }
    }
}

struct RowCountChange {
    /// History lines a rows-grow revealed at the top.
    revealed: usize,
    /// The revealed rows' extras, `(viewport row, extras)`, to re-inject after the caller's shift.
    extras: Vec<(u16, Box<ScrolledRowExtras>)>,
    /// Rows the grow appended blank for history a detach window's job holds.
    owed: usize,
}

impl RowCountChange {
    const fn revealed(revealed: usize, extras: Vec<(u16, Box<ScrolledRowExtras>)>) -> Self {
        Self {
            revealed,
            extras,
            owed: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_clamps_oversize_dimensions() {
        for reflow in [true, false] {
            let mut grid = Grid::new(5, 10);
            if reflow {
                grid.resize(u16::MAX, u16::MAX);
            } else {
                grid.resize_no_reflow(u16::MAX, u16::MAX);
            }
            assert_eq!((grid.rows(), grid.cols()), (MAX_GRID_ROWS, MAX_GRID_COLS));
            grid.assert_invariants();
        }
    }

    #[test]
    fn reflow_shrink_to_1_column() {
        let mut grid = Grid::new(5, 4);
        for c in "ABCD".chars() {
            grid.write_char(c);
        }
        grid.set_cursor(0, 0);

        grid.resize(5, 1);

        assert_eq!(grid.row(0).unwrap().to_string(), "A");
        assert_eq!(grid.row(1).unwrap().to_string(), "B");
        assert_eq!(grid.row(2).unwrap().to_string(), "C");
        assert_eq!(grid.row(3).unwrap().to_string(), "D");
        assert!(grid.row(1).unwrap().is_wrapped());
        assert!(grid.row(2).unwrap().is_wrapped());
        assert!(grid.row(3).unwrap().is_wrapped());
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

        grid.resize_with_reflow_mode(5, 20, ReflowMode::Disabled);

        assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
        assert_eq!(grid.row(1).unwrap().to_string(), "FGHIJ");
        grid.assert_invariants();
    }

    fn ten_lines_in_four_rows() -> Grid {
        let mut grid = Grid::with_scrollback(4, 10, 100);
        for i in 0..10 {
            grid.write_char('L');
            grid.write_char(char::from(b'0' + i));
            grid.line_feed();
            grid.carriage_return();
        }
        assert_eq!(grid.scrollback_lines(), 7);
        assert_eq!(grid.row(0).unwrap().to_string(), "L7");
        assert_eq!(grid.cursor_row(), 3);
        grid
    }

    #[test]
    fn conpty_rows_grow_appends_blanks_and_reveals_nothing() {
        let mut grid = ten_lines_in_four_rows();
        let epoch = grid.history_renumber_epoch();
        let reveal_gen = grid.storage.history_reveal_gen;
        let oldest = grid.oldest_absolute_row();
        let top = grid.visible_to_absolute(0);
        let counter = grid.absolute_row_counter();

        grid.resize_with_policy(8, 10, ResizePolicy::ConPty);

        assert_eq!(grid.rows(), 8);
        assert_eq!(grid.scrollback_lines(), 7, "no history revealed");
        assert_eq!(grid.row(0).unwrap().to_string(), "L7");
        assert_eq!(grid.row(1).unwrap().to_string(), "L8");
        assert_eq!(grid.row(2).unwrap().to_string(), "L9");
        for r in 3..8 {
            assert!(grid.row(r).unwrap().is_empty(), "row {r} is a fresh blank");
        }
        assert_eq!(grid.cursor_row(), 3, "cursor row unchanged");
        assert_eq!(grid.take_last_resize_row_shift(), 0, "nothing moved");
        assert_eq!(grid.oldest_absolute_row(), oldest, "history keys kept");
        assert_eq!(grid.visible_to_absolute(0), top, "`L7` keeps its key");
        assert_eq!(
            grid.absolute_row_counter(),
            counter + 4,
            "the four appended rows took fresh keys"
        );
        assert_eq!(
            grid.history_renumber_epoch(),
            epoch,
            "no key slid, so nothing was renumbered"
        );
        assert_eq!(
            grid.storage.history_reveal_gen, reveal_gen,
            "nothing was revealed, so the reveal fence does not move"
        );
        assert_eq!(
            grid.get_history_line(6).map(|l| l.to_string()),
            Some("L6".to_string()),
            "the newest history line is still history"
        );
        grid.assert_invariants();
    }

    #[test]
    fn conpty_rows_grow_then_trim_keeps_every_key() {
        let mut grid = ten_lines_in_four_rows();
        let epoch = grid.history_renumber_epoch();
        let keys = |g: &Grid| (g.oldest_absolute_row(), g.visible_to_absolute(0));
        let before = keys(&grid);
        let counter = grid.absolute_row_counter();

        grid.resize_with_policy(8, 10, ResizePolicy::ConPty);
        assert_eq!(keys(&grid), before, "after the grow");
        grid.resize_with_policy(4, 10, ResizePolicy::ConPty);
        assert_eq!(keys(&grid), before, "after the trim");
        assert_eq!(grid.scrollback_lines(), 7, "the trim demoted nothing");
        assert_eq!(grid.row(0).unwrap().to_string(), "L7");
        assert_eq!(grid.absolute_row_counter(), counter, "the counter is back");
        assert_eq!(grid.history_renumber_epoch(), epoch);
        grid.assert_invariants();
    }

    #[test]
    fn conpty_rows_grow_inside_an_offload_window_keeps_the_reader_aim() {
        use aterm_scrollback::{Scrollback, ScrollbackStorage};
        let rows = 10u16;
        let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
        let mut grid = Grid::with_tiered_scrollback(rows, 80, 8, sb);
        for i in 0..500 {
            grid.set_cursor(rows - 1, 0);
            for c in format!("H{i}").chars() {
                grid.write_char(c);
            }
            grid.line_feed();
            grid.carriage_return();
        }
        let pending = grid
            .resize_offloading_scrollback_with_policy(rows, 60, ResizePolicy::ConPty)
            .expect("a width change with a tiered store offloads");
        grid.scroll_display(30);
        assert!(grid.reader_aim_held(), "precondition: an aim past the top");

        assert!(
            grid.resize_offloading_scrollback_with_policy(16, 60, ResizePolicy::ConPty)
                .is_none(),
            "a rows-only grow offloads nothing"
        );
        grid.reattach_reflowed_scrollback(pending.reflow());
        assert_eq!(grid.display_offset(), 30, "the view lands where they asked");
        grid.assert_invariants();
    }

    #[test]
    fn native_rows_grow_still_reveals_history() {
        let mut grid = ten_lines_in_four_rows();
        let reveal_gen = grid.storage.history_reveal_gen;
        grid.resize(8, 10);
        assert_eq!(grid.scrollback_lines(), 3);
        assert_eq!(grid.row(0).unwrap().to_string(), "L3");
        assert_eq!(grid.cursor_row(), 7);
        assert_eq!(grid.take_last_resize_row_shift(), 4);
        assert_eq!(
            grid.storage.history_reveal_gen,
            reveal_gen.wrapping_add(1),
            "the reveal moves the history carry's fence"
        );
        grid.assert_invariants();
    }

    fn boundary_straddling_line() -> Grid {
        let mut grid = Grid::with_scrollback(3, 10, 100);
        for c in "ABCDEFGHIJ".chars() {
            grid.write_char(c);
        }
        grid.line_feed();
        grid.carriage_return();
        for c in "KLMNO".chars() {
            grid.write_char(c);
        }
        grid.row_mut(1).unwrap().set_wrapped(true);
        grid.line_feed();
        grid.carriage_return();
        grid.write_char('x');
        grid.line_feed();
        grid.carriage_return();
        grid.write_char('y');
        assert_eq!(grid.scrollback_lines(), 1);
        assert_eq!(
            grid.get_history_line(0).map(|l| l.to_string()),
            Some("ABCDEFGHIJ".to_string())
        );
        assert_eq!(grid.row(0).unwrap().to_string(), "KLMNO");
        assert!(grid.row(0).unwrap().is_wrapped());
        assert_eq!((grid.cursor_row(), grid.cursor_col()), (2, 1));
        grid
    }

    #[test]
    fn conpty_widen_skips_the_boundary_lift_and_the_deficit_fill() {
        let mut grid = boundary_straddling_line();
        grid.resize_with_policy(3, 20, ResizePolicy::ConPty);
        assert_eq!(grid.scrollback_lines(), 1, "the head is not lifted");
        assert_eq!(
            grid.get_history_line(0).map(|l| l.to_string()),
            Some("ABCDEFGHIJ".to_string()),
            "history rewraps only itself"
        );
        assert_eq!(grid.row(0).unwrap().to_string(), "KLMNO");
        assert!(
            !grid.row(0).unwrap().is_wrapped(),
            "severed from its head, as conhost treats it"
        );
        assert_eq!(grid.row(1).unwrap().to_string(), "x");
        assert_eq!(grid.row(2).unwrap().to_string(), "y");
        assert_eq!((grid.cursor_row(), grid.cursor_col()), (2, 1));
        grid.assert_invariants();
    }

    #[test]
    fn conpty_width_round_trip_keeps_the_history_line_count() {
        let mut grid = boundary_straddling_line();
        for (step, cols) in [20u16, 10, 20, 10].into_iter().enumerate() {
            grid.resize_with_policy(3, cols, ResizePolicy::ConPty);
            assert_eq!(grid.scrollback_lines(), 1, "step {step} ({cols} cols)");
            assert_eq!(
                grid.get_history_line(0).map(|l| l.to_string()),
                Some("ABCDEFGHIJ".to_string()),
                "step {step}: the head, and nothing after it"
            );
            assert_eq!(grid.row(0).unwrap().to_string(), "KLMNO", "step {step}");
            grid.assert_invariants();
        }
    }

    #[test]
    fn native_widen_still_rejoins_and_refills() {
        let mut grid = boundary_straddling_line();
        grid.resize(3, 20);
        assert_eq!(
            grid.scrollback_lines(),
            0,
            "the rejoined line was pulled back in"
        );
        assert_eq!(grid.row(0).unwrap().to_string(), "ABCDEFGHIJKLMNO");
        assert_eq!(grid.row(1).unwrap().to_string(), "x");
        assert_eq!(grid.row(2).unwrap().to_string(), "y");
        grid.assert_invariants();
    }

    #[test]
    fn conpty_rows_shrink_equals_native() {
        let mut native = ten_lines_in_four_rows();
        let mut conpty = ten_lines_in_four_rows();
        native.resize(2, 10);
        conpty.resize_with_policy(2, 10, ResizePolicy::ConPty);
        assert_eq!(native.scrollback_lines(), conpty.scrollback_lines());
        for r in 0..2 {
            assert_eq!(
                native.row(r).unwrap().to_string(),
                conpty.row(r).unwrap().to_string()
            );
        }
        assert_eq!(native.cursor_row(), conpty.cursor_row());
        assert_eq!(
            native.history_renumber_epoch(),
            conpty.history_renumber_epoch()
        );
        conpty.assert_invariants();
    }

    fn labelled_screen(rows: u16, cursor_row: u16, max_scrollback: usize) -> Grid {
        let mut grid = Grid::with_scrollback(rows, 10, max_scrollback);
        for r in 0..rows {
            grid.set_cursor(r, 0);
            for c in format!("R{r}").chars() {
                grid.write_char(c);
            }
        }
        grid.set_cursor(cursor_row, 0);
        grid
    }

    #[test]
    fn last_resize_shape_in_the_trim_demote_and_bottom_push_regimes() {
        use crate::ResizeShape;
        let shape = |trimmed, demoted, pushed, revealed, appended| ResizeShape {
            trimmed,
            demoted,
            pushed,
            revealed,
            appended,
            ..ResizeShape::default()
        };

        let mut grid = labelled_screen(2, 1, 100);
        grid.resize(6, 10);
        assert_eq!(grid.take_last_resize_shape(), shape(0, 0, 0, 0, 4));
        grid.resize(4, 10);
        assert_eq!(grid.last_resize_shape(), shape(2, 0, 0, 0, 0), "peek");
        assert_eq!(grid.take_last_resize_shape(), shape(2, 0, 0, 0, 0));
        assert_eq!(grid.last_resize_shape(), ResizeShape::default(), "drained");
        assert_eq!(grid.take_last_resize_row_shift(), 0, "a trim moves nothing");
        assert_eq!(grid.row(0).unwrap().to_string(), "R0");

        let mut grid = labelled_screen(6, 5, 100);
        grid.erase_line();
        grid.set_cursor(4, 0);
        grid.resize(3, 10);
        assert_eq!(grid.take_last_resize_shape(), shape(1, 2, 0, 0, 0));
        assert_eq!(grid.row(0).unwrap().to_string(), "R2");
        assert_eq!(grid.cursor_row(), 2);

        let mut grid = labelled_screen(6, 4, 100);
        grid.resize(4, 10);
        assert_eq!(grid.take_last_resize_shape(), shape(0, 2, 0, 0, 0));
        assert_eq!(
            grid.take_last_resize_row_shift(),
            2,
            "the selection's input"
        );
        assert_eq!(grid.row(0).unwrap().to_string(), "R2");
        grid.resize(6, 10);
        assert_eq!(grid.take_last_resize_shape(), shape(0, 0, 0, 2, 0));
        assert_eq!(grid.row(0).unwrap().to_string(), "R0");
        assert_eq!(grid.row(5).unwrap().to_string(), "R5");
        assert_eq!(grid.cursor_row(), 4);

        let mut grid = labelled_screen(6, 4, 0);
        grid.resize(4, 10);
        assert_eq!(
            grid.take_last_resize_shape(),
            ResizeShape {
                stashed: 2,
                ..shape(0, 2, 0, 0, 0)
            }
        );
        assert_eq!(grid.scrollback_lines(), 0, "retention 0 evicted the demote");
        grid.resize(6, 10);
        assert_eq!(
            grid.take_last_resize_shape(),
            ResizeShape {
                restored_top: 2,
                undone: true,
                ..shape(0, 0, 0, 2, 0)
            }
        );
        assert_eq!(grid.row(0).unwrap().to_string(), "R0");
        assert_eq!(grid.row(5).unwrap().to_string(), "R5");
        assert_eq!(grid.cursor_row(), 4);
        assert_eq!(grid.scrollback_lines(), 0, "handed back, not retained");

        let mut grid = labelled_screen(6, 4, 0);
        grid.resize(4, 10);
        grid.write_char('x');
        grid.resize(6, 10);
        assert_eq!(grid.take_last_resize_shape(), shape(0, 0, 0, 0, 2));
        assert_eq!(grid.row(0).unwrap().to_string(), "R2");
        assert_eq!(grid.row(3).unwrap().to_string(), "R5");
        assert!(grid.row(4).unwrap().is_empty());
        assert!(grid.row(5).unwrap().is_empty());
        assert_eq!(grid.cursor_row(), 2);

        let mut grid = labelled_screen(6, 0, 100);
        grid.resize(4, 10);
        assert_eq!(grid.take_last_resize_shape(), shape(0, 0, 2, 0, 0));
        assert_eq!(grid.row(0).unwrap().to_string(), "R0", "the top stays put");

        let mut grid = labelled_screen(6, 1, 100);
        grid.resize(4, 10);
        assert_eq!(grid.take_last_resize_shape(), shape(0, 1, 1, 0, 0));

        grid.resize(4, 10);
        grid.resize(4, 10);
        assert_eq!(grid.take_last_resize_shape(), ResizeShape::default());

        let mut grid = labelled_screen(6, 4, 100);
        grid.resize(6, 12);
        assert!(grid.take_last_resize_shape().reflowed);
        grid.resize_no_reflow(6, 10);
        assert!(!grid.take_last_resize_shape().reflowed);

        let mut conpty = ten_lines_in_four_rows();
        conpty.resize_with_policy(8, 10, ResizePolicy::ConPty);
        assert_eq!(conpty.take_last_resize_shape(), shape(0, 0, 0, 0, 4));
        let mut native = ten_lines_in_four_rows();
        native.resize(8, 10);
        assert_eq!(native.take_last_resize_shape(), shape(0, 0, 0, 4, 0));
        native.assert_invariants();
        conpty.assert_invariants();
    }

    #[test]
    fn a_narrowing_with_no_rewrap_counts_the_drawn_columns_it_cuts() {
        let mut grid = labelled_screen(4, 0, 0);
        let draw = |grid: &mut Grid, row: u16, text: &str| {
            grid.set_cursor(row, 0);
            for c in text.chars() {
                grid.write_char(c);
            }
        };
        draw(&mut grid, 1, "0123456");
        draw(&mut grid, 2, "abcdefgh");
        draw(&mut grid, 3, "xy        ");
        grid.resize_no_reflow(4, 9);
        assert_eq!(grid.take_last_resize_shape().cut, 0, "only blanks past 9");
        grid.resize_no_reflow(4, 6);
        assert_eq!(
            grid.take_last_resize_shape().cut,
            2,
            "row 2 drew to column 7"
        );
        grid.resize_no_reflow(4, 10);
        assert_eq!(grid.take_last_resize_shape().cut, 0, "a widening");
        assert_eq!(grid.row(2).unwrap().to_string(), "abcdef", "the cut cells");

        let mut grid = labelled_screen(4, 0, 100);
        draw(&mut grid, 1, "abcdefghij");
        grid.resize(4, 6);
        let shape = grid.take_last_resize_shape();
        assert!(shape.reflowed);
        assert_eq!(shape.cut, 0, "a rewrap keeps every cell");
    }

    fn prompt_above_a_non_blank_row() -> Grid {
        let mut grid = Grid::with_scrollback(6, 10, 100);
        for i in 0..10 {
            grid.write_char('L');
            grid.write_char(char::from(b'0' + i));
            grid.line_feed();
            grid.carriage_return();
        }
        grid.write_char('>');
        grid.line_feed();
        grid.carriage_return();
        grid.write_char('>');
        grid.write_char('>');
        grid.move_cursor_to(4, 1);
        assert_eq!(grid.scrollback_lines(), 6);
        assert_eq!(grid.row(4).unwrap().to_string(), ">");
        assert_eq!(grid.row(5).unwrap().to_string(), ">>");
        grid
    }

    fn history_text(grid: &Grid) -> Vec<String> {
        (0..grid.scrollback_lines())
            .map(|i| grid.get_history_line(i).unwrap().to_string())
            .collect()
    }

    fn screen_text(grid: &Grid) -> Vec<String> {
        (0..grid.rows())
            .map(|r| grid.row(r).unwrap().to_string())
            .collect()
    }

    #[test]
    fn conpty_width_shrink_demotes_the_excess_above_a_non_blank_row() {
        let lines = |r: std::ops::RangeInclusive<u8>| -> Vec<String> {
            r.map(|i| format!("L{i}")).collect()
        };
        let mut conpty = prompt_above_a_non_blank_row();
        conpty.resize_with_policy(4, 8, ResizePolicy::ConPty);
        assert_eq!(
            history_text(&conpty),
            lines(0..=7),
            "both excess rows demoted"
        );
        assert_eq!(screen_text(&conpty), ["L8", "L9", ">", ">>"]);
        assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (2, 1));
        conpty.assert_invariants();

        let mut native = prompt_above_a_non_blank_row();
        native.resize(4, 8);
        assert_eq!(
            history_text(&native),
            lines(0..=7),
            "Native: the window's end stays on screen"
        );
        assert_eq!(screen_text(&native), ["L8", "L9", ">", ">>"]);
        assert_eq!((native.cursor_row(), native.cursor_col()), (2, 1));
        native.assert_invariants();

        let mut one_pass = prompt_above_a_non_blank_row();
        one_pass.forget_handback();
        one_pass.resize(4, 8);
        assert_eq!(
            history_text(&one_pass),
            lines(0..=6),
            "one pass: the cursor's minimum"
        );
        assert_eq!(screen_text(&one_pass), ["L7", "L8", "L9", ">"]);
        assert_eq!(one_pass.cursor_row(), 3);
    }

    #[test]
    fn conpty_width_shrink_demotes_no_further_than_the_cursor_and_drops_blanks() {
        let mut blank = Grid::with_scrollback(3, 10, 100);
        for line in ["0123456789", "abcdefghij"] {
            for c in line.chars() {
                blank.write_char_wrap(c);
            }
            blank.carriage_return();
            blank.line_feed();
        }
        assert_eq!(blank.cursor_row(), 2);
        blank.resize_with_policy(3, 5, ResizePolicy::ConPty);
        assert_eq!(history_text(&blank), ["01234", "56789"]);
        assert_eq!(screen_text(&blank), ["abcde", "fghij", ""]);
        assert_eq!((blank.cursor_row(), blank.cursor_col()), (2, 0));
        blank.assert_invariants();

        let blank_tail = || {
            let mut grid = Grid::with_scrollback(4, 10, 100);
            for line in ["L0", ">"] {
                for c in line.chars() {
                    grid.write_char_wrap(c);
                }
                grid.carriage_return();
                grid.line_feed();
            }
            for c in "abcdefghijk".chars() {
                grid.write_char_wrap(c);
            }
            grid.move_cursor_to(3, 0);
            grid.erase_chars(1);
            grid.move_cursor_to(2, 5);
            grid.delete_chars(5);
            grid.move_cursor_to(1, 1);
            let tail = grid.row(3).unwrap();
            assert!(tail.is_empty() && tail.is_wrapped());
            grid
        };
        let mut one_step = blank_tail();
        one_step.resize_with_policy(3, 5, ResizePolicy::ConPty);
        let mut two_steps = blank_tail();
        two_steps.resize_with_policy(4, 5, ResizePolicy::ConPty);
        two_steps.resize_with_policy(3, 5, ResizePolicy::ConPty);
        assert_eq!(history_text(&one_step), ["L0"]);
        assert_eq!(history_text(&one_step), history_text(&two_steps));
        assert_eq!(screen_text(&one_step), screen_text(&two_steps));
        assert_eq!(one_step.cursor_row(), two_steps.cursor_row());

        let mut high = prompt_above_a_non_blank_row();
        high.move_cursor_to(1, 0);
        high.resize_with_policy(3, 8, ResizePolicy::ConPty);
        assert_eq!(high.scrollback_lines(), 7);
        assert_eq!(high.row(0).unwrap().to_string(), "L7");
        assert_eq!(high.cursor_row(), 0);
        high.assert_invariants();

        let mut conpty = prompt_above_a_non_blank_row();
        let mut native = prompt_above_a_non_blank_row();
        for grid in [&mut conpty, &mut native] {
            grid.move_cursor_to(5, 0);
            grid.erase_line();
            grid.move_cursor_to(4, 1);
        }
        conpty.resize_with_policy(4, 8, ResizePolicy::ConPty);
        native.resize(4, 8);
        assert_eq!(history_text(&conpty), history_text(&native));
        assert_eq!(screen_text(&conpty), ["L7", "L8", "L9", ">"]);
        assert_eq!(conpty.cursor_row(), native.cursor_row());
    }

    fn prompt_with_the_cursor_past_its_end() -> Grid {
        let mut grid = Grid::with_scrollback(6, 10, 100);
        for i in 0..10 {
            grid.write_char('L');
            grid.write_char(char::from(b'0' + i));
            grid.line_feed();
            grid.carriage_return();
        }
        for c in "PPPPPPPPPPQQQQQ".chars() {
            grid.write_char_wrap(c);
        }
        grid.move_cursor_to(5, 6);
        assert_eq!(grid.scrollback_lines(), 6);
        assert_eq!(screen_text(&grid)[4..], ["PPPPPPPPPP", "QQQQQ"]);
        assert!(grid.row(5).unwrap().is_wrapped());
        grid
    }

    #[test]
    fn conpty_rewrap_keeps_the_cursor_cell_past_the_prompt_as_a_row() {
        let lines = |r: std::ops::RangeInclusive<u8>| -> Vec<String> {
            r.map(|i| format!("L{i}")).collect()
        };
        let mut conpty = prompt_with_the_cursor_past_its_end();
        conpty.resize_with_policy(6, 5, ResizePolicy::ConPty);
        assert_eq!(history_text(&conpty), lines(0..=7), "two rows demoted");
        assert_eq!(
            screen_text(&conpty),
            ["L8", "L9", "PPPPP", "PPPPP", "QQQQQ", ""]
        );
        assert!(
            conpty.row(5).unwrap().is_wrapped(),
            "the cursor's row continues the prompt"
        );
        assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (5, 1));
        conpty.assert_invariants();

        conpty.resize_with_policy(6, 10, ResizePolicy::ConPty);
        assert_eq!(history_text(&conpty), lines(0..=7));
        assert_eq!(
            screen_text(&conpty),
            ["L8", "L9", "PPPPPPPPPP", "QQQQQ", "", ""]
        );
        assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (3, 6));
        conpty.assert_invariants();

        // Native carries the cursor's cell the same way, so the next glyph goes where it would
        // have before the resize rather than onto the last Q.
        let mut native = prompt_with_the_cursor_past_its_end();
        native.resize(6, 5);
        assert_eq!(history_text(&native), lines(0..=7));
        assert_eq!(
            screen_text(&native),
            ["L8", "L9", "PPPPP", "PPPPP", "QQQQQ", ""]
        );
        assert!(native.row(5).unwrap().is_wrapped());
        assert_eq!(
            (
                native.cursor_row(),
                native.cursor_col(),
                native.pending_wrap()
            ),
            (5, 1, false)
        );
        native.assert_invariants();
    }

    #[test]
    fn conpty_rewrap_keeps_the_cursor_offset_on_an_unwrapped_line() {
        let prompt = || {
            let mut grid = Grid::new(3, 10);
            for c in "PPPPPP".chars() {
                grid.write_char(c);
            }
            grid.move_cursor_to(0, 8);
            grid
        };
        let mut conpty = prompt();
        conpty.resize_with_policy(3, 4, ResizePolicy::ConPty);
        assert_eq!(screen_text(&conpty), ["PPPP", "PP", ""]);
        assert!(conpty.row(2).unwrap().is_wrapped());
        assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (2, 0));
        conpty.assert_invariants();
        // Native carries the cursor's cell too: offset 8 is past the line's last row, so the
        // cursor starts a row that continues the line, where its next glyph lands.
        let mut native = prompt();
        native.resize(3, 4);
        assert_eq!(screen_text(&native), ["PPPP", "PP", ""]);
        assert!(native.row(2).unwrap().is_wrapped());
        assert_eq!(
            (
                native.cursor_row(),
                native.cursor_col(),
                native.pending_wrap()
            ),
            (2, 0, false)
        );
        native.write_char_wrap('X');
        assert_eq!(screen_text(&native), ["PPPP", "PP", "X"]);
        native.assert_invariants();

        let mut conpty = prompt();
        conpty.resize_with_policy(3, 5, ResizePolicy::ConPty);
        assert_eq!(screen_text(&conpty), ["PPPPP", "P", ""]);
        assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (1, 3));
        conpty.assert_invariants();
        let mut native = prompt();
        native.resize(3, 5);
        assert_eq!(
            (
                native.cursor_row(),
                native.cursor_col(),
                native.pending_wrap()
            ),
            (1, 3, false)
        );
    }

    #[test]
    fn conpty_rewrap_carries_a_cursor_on_a_blank_row_past_the_width() {
        let parked = || {
            let mut grid = Grid::new(24, 80);
            grid.move_cursor_to(5, 70);
            grid
        };
        let mut conpty = parked();
        conpty.resize_with_policy(24, 60, ResizePolicy::ConPty);
        assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (6, 10));
        assert!(conpty.row(6).unwrap().is_wrapped());
        conpty.resize_with_policy(24, 40, ResizePolicy::ConPty);
        assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (6, 30));
        conpty.resize_with_policy(24, 80, ResizePolicy::ConPty);
        assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (5, 70));
        assert!((0..24).all(|r| conpty.row(r).unwrap().is_empty()));
        assert_eq!(conpty.scrollback_lines(), 0);
        conpty.assert_invariants();

        let mut native = parked();
        native.resize(24, 60);
        assert_eq!((native.cursor_row(), native.cursor_col()), (6, 10));
        assert!(native.row(6).unwrap().is_wrapped());
        native.resize(24, 80);
        assert_eq!((native.cursor_row(), native.cursor_col()), (5, 70));
        assert!((0..24).all(|r| native.row(r).unwrap().is_empty()));
        native.assert_invariants();
    }
}
