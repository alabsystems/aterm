// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grid reflow: rewrap lines when terminal column count changes.
//!
//! O(rows × cols) complexity, verified by the `reflow_linear_time*` tests.

#[path = "reflow_map.rs"]
mod reflow_map;

use self::reflow_map::{
    ExtrasCopyCtx, ExtrasSource, TrackedPoint, chunk_cells_to_rows, copy_cells_to_row,
    source_coords_for_row,
};
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

/// Who repaints the viewport after a resize: the row accounting at the
/// history/viewport seam must agree with it, or a line is duplicated on one
/// side of the seam or lost on the other.
///
/// Under a Unix PTY nobody repaints: the grid is the frame. Under Windows
/// ConPTY conhost keeps its own buffer and, after every
/// `ResizePseudoConsole`, repaints the whole viewport from it, anchored at
/// row 0 (`CSI ?25l`, `CSI H`, then each row as `<text> CSI K CR LF`, a
/// multi-row logical line as one autowrapping run, then a CUP to its cursor):
///
/// * rows-grow: row 0 stays row 0, grown rows are painted `CSI K`; nothing is
///   revealed from scrollback and the cursor row is unchanged.
/// * rows-shrink: trailing blank rows are dropped, then the top rows are
///   demoted to scrollback; with content below the cursor, conhost demotes
///   only the rows above the cursor and cuts the rest off the bottom. The
///   Native shrink (trim, top-demote, bottom-push the remainder into history)
///   leaves the same viewport and keeps the cut rows in history. A width
///   change leaving more rows than fit demotes by the same rule, counted on
///   the rewrapped rows with the cursor's row kept through the cursor's cell
///   (`cursor_row_copy_len`, `Grid::conpty_reflow_demote`).
/// * widen: viewport lines unwrap in place, freed bottom rows are painted
///   `CSI K`, no history is pulled in. A wrap continuation at row 0 (head in
///   history) is repainted as a standalone fragment and never rejoined with
///   its head in either direction.
/// * conhost's output sets continuation flags the rewrap trusts, and pwsh's
///   `cls` is `CSI K` down every row, which must break each row's link to the
///   next (`Grid::clear_wrap_into_next_row`).
///
/// So the Native policy's seam-crossing moves (revealing history into grown
/// rows, lifting the boundary continuation into the history rewrap, pulling
/// history back to bottom-anchor a widened viewport) are exactly what
/// conhost's repaint paints over, losing or duplicating lines. The `ConPty`
/// policy makes none of them and, on a width change, severs the row-0
/// fragment from its head (`sever_top_row_continuation`).
///
/// Absolute row keys: under both policies a grow's bottom append and a shrink's
/// trailing-blank trim move `absolute_row_counter` with their rows
/// (`keep_keys_across_bottom_rows`), so every retained row keeps its key.
/// Holding the counter fixed would slide every key (OSC 133 marks, output
/// blocks, annotations) by the height delta, unseen when there is no history
/// for `oldest_absolute_row()` to move by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ResizePolicy {
    /// The grid is the frame (Unix PTYs, replays, tests): a rows-grow reveals
    /// retained history at the top, a width change bottom-anchors the viewport
    /// (boundary-continuation lift + deficit fill).
    #[default]
    Native,
    /// conhost repaints the viewport after every resize (above): a rows-grow
    /// appends blank rows at the bottom and reveals nothing; a width change
    /// rewraps the viewport and the off-screen history separately, with no
    /// continuation lift and no deficit fill; a continuation at row 0 becomes a
    /// line of its own; and the cursor's row keeps every cell through the
    /// cursor's, so the cursor keeps its offset in its line where Native clamps
    /// it to the last glyph. A rows-only shrink keeps the Native shape; a shrink
    /// that rewraps demotes that same count from the rewrapped rows, where
    /// Native pushes only what brings the cursor on screen.
    ConPty,
}

/// Rows a rows-only resize added at, or dropped from, the bottom of the
/// buffer: what `Grid::keep_keys_across_bottom_rows` moves the counter by.
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
    /// Where the saved (DECSC) cursor landed, when it named a rewrapped row.
    saved: Option<(usize, u16)>,
}

/// The cursors a width rewrap carries to their new cells (as kitty's
/// `CursorTrack`s and Ghostty's tracked pins do): [`LIVE`] is the live cursor,
/// [`SAVED`] the saved one (DECSC, or the 1049 slot the host lends). Each is a
/// source `(row, col)` before the rewrap and the `(row, col)` its cell lands on.
struct RewrapCursors {
    src: [Option<(usize, u16)>; 2],
    dst: [Option<(usize, u16)>; 2],
}

const LIVE: usize = 0;
const SAVED: usize = 1;

impl RewrapCursors {
    /// The cursors on source row `row`: their columns, by index.
    fn cols_on(&self, row: usize) -> [Option<u16>; 2] {
        self.src
            .map(|src| src.and_then(|(r, c)| (r == row).then_some(c)))
    }

    /// A row that is not rewrapped (blank, or DECDWL/DECDHL in place) keeps
    /// its cursors on its one new row, clamped to the new width.
    fn place_on_row(&mut self, row: usize, new_row: usize, new_cols: u16) {
        for (i, col) in self.cols_on(row).into_iter().enumerate() {
            if let Some(col) = col {
                self.dst[i] = Some((new_row, col.min(new_cols.saturating_sub(1))));
            }
        }
    }

    /// Tracked points for a line chunked from `cells`: each cursor's logical
    /// offset into the line, from the `offsets` its rows give it. The saved
    /// cursor keeps a gap past the line's content (`TrackedPoint::keeps_gap`).
    fn points(offsets: [Option<usize>; 2]) -> [TrackedPoint; 2] {
        let [live, saved] = offsets;
        [
            TrackedPoint {
                offset: live,
                at: None,
                keeps_gap: false,
            },
            TrackedPoint {
                offset: saved,
                at: None,
                keeps_gap: true,
            },
        ]
    }

    /// Take the chunker's answers.
    fn land(&mut self, points: &[TrackedPoint; 2]) {
        for (i, point) in points.iter().enumerate() {
            if let Some(at) = point.at {
                self.dst[i] = Some(at);
            }
        }
    }
}

/// May the rewrap merge consume `row` as a soft-wrap continuation of the
/// preceding row? Not a DECDWL/DECDHL row: the merged chunk inherits only the
/// first source row's `line_size`, so it resizes in place instead.
fn is_mergeable_continuation(row: &Row) -> bool {
    row.is_wrapped() && row.line_size() == LineSize::SingleWidth
}

/// How many cells of the cursor's row a ConPTY rewrap copies: its content and
/// every cell through the cursor's, blank or not, as conhost's Reflow copies
/// through `max(MeasureRight, cursor.x + 1)`. A cursor parked past the last
/// glyph (pwsh's prompt ends in `CSI 1 C`) keeps its offset in the line, so
/// when the cells before it end on a row boundary its cell starts a row of its
/// own. Clamping to the last glyph would count one row fewer than conhost
/// demotes and let its row-0 repaint overwrite a line.
fn cursor_row_copy_len(row: &Row, cursor_col: u16) -> usize {
    usize::from(row.len())
        .max(usize::from(cursor_col) + 1)
        .min(row.as_slice().len())
}

/// Resize a DECDWL/DECDHL row in place (truncate or pad) without reflow:
/// each character occupies two physical columns, so reflowing would split the
/// logical line.
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
    // SAFETY: `new_row` is appended to `new_rows` and returned alongside
    // `new_pages` in the same reflow result.
    let mut new_row = unsafe { Row::new(new_cols, new_pages) };
    // Set line_size before copying cells: set_line_size(DoubleWidth) clears
    // cells from cols/2 onward, which copy_cells_to_row then overwrites.
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
    // COST: UNBOUNDED(scrollback-width-reflow) — the width branch rewraps the
    // whole scrollback synchronously; on the main thread under the `term` lock
    // that can freeze the UI (`aterm-census --mainloop`). The offloaded path
    // detaches history first.
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

    /// [`Grid::resize`] under an explicit [`ResizePolicy`]: reflow enabled, with
    /// the seam accounting the host's repaint behaviour requires. `Native` is
    /// exactly `resize`.
    pub fn resize_with_policy(&mut self, new_rows: u16, new_cols: u16, policy: ResizePolicy) {
        self.resize_with_reflow_mode_and_policy(new_rows, new_cols, ReflowMode::Enabled, policy);
    }

    /// [`Grid::resize_no_reflow`] under an explicit [`ResizePolicy`]: an alt grid
    /// never rewraps but can hold ring history (a TUI that scrolled a region),
    /// and conhost repaints the alt viewport as it does the main one.
    pub fn resize_no_reflow_with_policy(
        &mut self,
        new_rows: u16,
        new_cols: u16,
        policy: ResizePolicy,
    ) {
        self.resize_with_reflow_mode_and_policy(new_rows, new_cols, ReflowMode::Disabled, policy);
    }

    /// Lend this grid the host's saved cursor (DECSC, or the 1049 slot) for the
    /// resize that follows, so the grid carries it with its text like its own
    /// cursor: through a width rewrap, the push of top rows into history, a
    /// rows-grow reveal, a rows-shrink top-demote, the boundary-continuation
    /// lift and the deficit fill (the offloaded re-attach's included). Take it
    /// back with [`Self::take_lent_saved_cursor`] once the resize (or re-attach)
    /// returns.
    ///
    /// The terminal keeps one slot per screen (xterm `sc[0]`/`sc[1]`) and lends
    /// each grid its own; xterm, kitty and Ghostty all move it with the text.
    /// Left behind, a full-screen app's 1049 exit would restore the cursor onto
    /// earlier output. A slot past the grid names no text and is left where it
    /// is; DECRC and the 1049 exit clamp what they read.
    pub fn lend_saved_cursor(&mut self, saved: Option<super::Cursor>) {
        self.storage.saved_cursor = crate::SavedCursor {
            cursor: saved.unwrap_or_default(),
            valid: saved.is_some(),
            pending_wrap: false,
        };
    }

    /// Take back the saved cursor lent by [`Self::lend_saved_cursor`], where
    /// the resize carried it; the grid keeps none of its own afterwards.
    pub fn take_lent_saved_cursor(&mut self) -> Option<super::Cursor> {
        let saved = std::mem::take(&mut self.storage.saved_cursor);
        saved.valid.then_some(saved.cursor)
    }

    /// The one resize implementation: every public resize forwards here. Same
    /// cost note as [`Self::resize_with_reflow_mode`].
    fn resize_with_reflow_mode_and_policy(
        &mut self,
        new_rows: u16,
        new_cols: u16,
        reflow_mode: ReflowMode,
        policy: ResizePolicy,
    ) {
        let reflow = matches!(reflow_mode, ReflowMode::Enabled);
        let conpty = policy == ResizePolicy::ConPty;
        // Ingress clamp: bound the allocation a hostile resize can request.
        let new_rows = new_rows.clamp(1, MAX_GRID_ROWS);
        let new_cols = new_cols.clamp(1, MAX_GRID_COLS);
        let old_cols = self.storage.cols;
        // First, before anything moves the cursor or margins: the resize undo
        // compares and keeps them.
        self.admit_resize_undo(new_rows, new_cols, policy);
        // This resize's row accounting starts empty: each arm of
        // `adjust_row_count` stores its own count below, and nothing an earlier
        // resize recorded may survive into this one's record (`ResizeShape`).
        self.storage.last_resize_shape = crate::ResizeShape {
            reflowed: new_cols != old_cols && reflow,
            ..crate::ResizeShape::default()
        };

        // Reflow runs at display_offset 0; the reader's position is restored at
        // the end, so a resize or font zoom does not yank them to the live bottom.
        let prev_offset = self.storage.display_offset;
        // The absolute row under the eye, captured before the offset is
        // cleared. Only while scrolled back: at offset 0 a height shrink would
        // otherwise scroll a live, tail-following viewport into history on every
        // drag.
        let prev_anchor = (prev_offset > 0).then(|| self.top_visible_absolute_row());
        self.storage.display_offset = 0;

        // Bottom anchoring: remember the viewport's trailing blank rows, which
        // the deficit fill at the end restores after a width-grow unwraps
        // content into fewer rows. Not on rows-only resizes (the fill's
        // renumbering would break the reveal's absolute numbering), and not
        // under ConPTY (conhost paints freed rows `CSI K`; a re-seated line
        // would be duplicated).
        let pre_trailing_blanks = (new_cols != old_cols && reflow && !conpty)
            .then(|| self.trailing_blank_rows_below_cursor());

        // On a width change with reflow, lift the off-screen scrollback out and
        // rewrap it to the new width before any visible-grid mutation, so
        // history survives the resize. Reads ring_extras, so it must precede the
        // ring_extras.clear() below. Restored after adjust_row_count.
        let reflowed_scrollback = if new_cols != old_cols && reflow {
            let mut old = self.take_scrollback_lines();
            // Lift the viewport's leading soft-wrap continuation rows (the tail
            // of a line whose head is the last history line) into the history
            // rewrap, so the straddling line rewraps as one unit; the deficit
            // fill below pulls the tail back. ConPTY instead severs the fragment
            // (see `sever_top_row_continuation`), after the take above read the
            // head at its full width.
            if conpty {
                self.sever_top_row_continuation();
            } else {
                old.extend(self.take_boundary_continuation_lines());
            }
            // Every line counted here was rewrapped synchronously under the
            // caller's lock, so it must stay bounded by the viewport, not by
            // history. The offloaded path (`resize_offloading_scrollback`)
            // drives it to zero.
            #[cfg(any(test, debug_assertions))]
            super::count_scrollback_reflow_sync_lines(old.len());
            let lines = super::scrollback_reflow::reflow_scrollback_lines(&old, new_cols);
            Some(lines)
        } else {
            None
        };

        // A rows-only resize reclassifies history in place on every grid shape
        // (`adjust_row_count_rows_only`), so its history extras and staged rows
        // survive; only a width change clears and drains them.
        let rows_only = new_cols == old_cols;
        if !rows_only {
            self.storage.ring_extras.clear();
            // Drain lazy buffer: deferred lines reference pre-reflow cell data.
            self.drain_lazy_buffer();
        }
        // What stays staged past that drain is a detach window's output; the
        // visible reflow stages its overflow behind it, and the restore below
        // tells the two apart by this count.
        let staged_before_reflow = self.storage.lazy_buffer.len();

        // The drain recycled cell bodies sized for `old_cols`: across a width
        // change they are dead memory or a guaranteed realloc.
        if new_cols != old_cols {
            self.storage.lazy_buffer.clear_pool();
        }

        let cursor_row = self.storage.cursor.row as usize;
        let cursor_col = self.storage.cursor.col;

        if new_cols != old_cols && reflow {
            self.reflow_columns(new_rows, new_cols, cursor_row, cursor_col, policy);
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
            // Discard extras beyond the new column count, which would otherwise
            // linger as orphaned entries.
            if new_cols < old_cols {
                self.storage.extras.retain_cols_below(new_cols);
                // `Row::resize` blanks a wide head the cut leaves without its
                // continuation; the head's entry (a cluster, a mark, a link)
                // goes with it, as for every blanked cell.
                for row in 0..self.storage.visible_rows {
                    self.drop_unclaimed_extras(row, new_cols - 1..new_cols);
                }
            }
        }

        // The column-reflow path migrates ring-stored non-BMP codepoints into
        // its fresh CellExtras. The other paths keep the extras and drop both
        // rings in `invalidate_rings` below, which would turn on-screen non-BMP
        // cells into U+FFFD: harvest them into the map first, at
        // pre-`adjust_row_count` positions so they ride the row shift.
        if !(new_cols != old_cols && reflow) {
            self.migrate_complex_ring_to_extras();
            self.migrate_rgb_ring_to_extras();
        }

        let (revealed, reveal_extras) = self.adjust_row_count(new_rows, new_cols, policy);
        if revealed > 0 {
            // History handed back to the screen is live again: fence every
            // absolute-row reader of history on it (`history_reveal_gen`). A
            // ConPTY rows-grow reveals nothing (`revealed == 0`), so it moves
            // neither this fence nor any key.
            self.storage.history_reveal_gen = self.storage.history_reveal_gen.wrapping_add(1);
        }
        // Discard CellExtras entries for rows removed by adjust_row_count.
        self.storage.extras.retain_rows_below(new_rows);
        // The rings' stride and row count are stale after any dimension change;
        // they are re-created on the next write.
        self.storage.extras.invalidate_rings();
        self.storage.resize_viewport_state(new_rows, new_cols);
        // A rows-grow that revealed history relabelled the newest ring lines as
        // the top of the viewport, so every pre-resize viewport row moved
        // `revealed` rows down; the cursor follows, or an inline TUI's
        // post-SIGWINCH repaint and CPR answers anchor wrong. The column-reflow
        // path tracks its own cursor (`reflow_columns`).
        if revealed > 0 && !(new_cols != old_cols && reflow) {
            // The selection lives on the Terminal: `Terminal::finalize_resize`
            // applies this shift.
            self.storage.last_resize_row_shift = row_u16(revealed);
            let bound = new_rows.saturating_sub(1);
            // Live extras follow their rows down too (the rings were
            // invalidated above; this shifts only the map).
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
            // The saved cursor moves with its row and is not clamped (DECRC
            // and the 1049 exit clamp what they read).
            if self.storage.saved_cursor.valid {
                self.storage.saved_cursor.cursor.row = self
                    .storage
                    .saved_cursor
                    .cursor
                    .row
                    .saturating_add(row_u16(revealed));
            }
        }
        // Re-attach the revealed ring rows' extras at their final viewport rows,
        // after the shift above so they do not ride it (see
        // `adjust_row_count_rows_only`).
        for (row, bx) in reveal_extras {
            self.inject_scrolled_extras(row, &bx);
        }
        // Restore the rewrapped history as the front (oldest) of the scrollback,
        // after the visible grid is finalized so adjust_row_count cannot trim it
        // and the new dimensions are in place.
        if let Some(lines) = reflowed_scrollback {
            self.restore_reflowed_scrollback(lines, new_cols, staged_before_reflow);
        }
        // Bottom-anchor the viewport: pull the newest history back until the
        // trailing-blank band matches its pre-resize count, rejoining the line
        // lifted above. The offloaded path holds the history on its worker, so
        // it fills at re-attach instead (`pending_fill_target`).
        if let Some(target) = pre_trailing_blanks {
            self.fill_viewport_deficit_from_history(target);
        }
        // Restore the reading position. `display_offset` counts from the live
        // bottom, so replaying it under a different `visible_rows` would slide
        // the content by the row delta on every height drag. A rows-only resize
        // keeps every archived line's absolute number, so the anchor is exact;
        // a width reflow renumbers rows, so the clamped offset is the best
        // answer there (staying in history beats snapping to the live bottom).
        // On the offloaded width path this clamp usually lands at 0 and
        // re-attach restores from `prev_offset`; a bare store assignment does
        // not advance `reader_live_bottom_gen`, so re-attach can tell it from a
        // reader who chose the live bottom.
        match prev_anchor {
            Some(anchor) if new_cols == old_cols => self.scroll_to_absolute_row(anchor),
            _ => self.storage.display_offset = prev_offset.min(self.storage.scrollback_lines()),
        }
        self.storage.pages.shrink_to_fit();
        self.storage.damage = Damage::Full;
        // Set directly, not via a `mark_content_*` wrapper: bump the generation
        // so a cached search index and change polls invalidate.
        self.storage.content_gen += 1;
        // A width rewrap renumbers rows wholesale: bump the epoch on every width
        // reflow, a narrowing with empty scrollback included (where the splice
        // sites all early-return). Absolute-row-keyed caches (the find overlay's
        // tint) read it; a double bump is harmless, a missed one silently wrong.
        if new_cols != old_cols && reflow {
            self.storage.history_renumber_epoch =
                self.storage.history_renumber_epoch.saturating_add(1);
        }
        // A rewrap that left more rows than the counter covers: raise it, so
        // the mark a shell records next reads back as its own row.
        self.cover_retained_rows();
        // Last, after this resize's own `content_gen` bump: the stash is valid
        // for the next resize only while nothing moves the generation, the
        // geometry or the cursor picture past what this one left.
        self.seal_resize_undo();
    }

    /// ConPTY width change: make the viewport's top row a line of its own
    /// instead of the continuation of the newest history line.
    ///
    /// conhost never rejoins such a row with its head (see [`ResizePolicy`]).
    /// Left set, the flag would also mislead `take_ring_scrollback_lines`, which
    /// reads a row whose successor is a continuation at its full width: true
    /// only at the width autowrap filled it at, so after a widen a later width
    /// change would read trailing blanks as content.
    ///
    /// Addresses row 0 in content coordinates (`ring_head` + history count),
    /// independent of `display_offset`. Called right after
    /// `take_scrollback_lines` has read the head at its full width.
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

    /// Copy every visible complex cell's ring-stored codepoint into the
    /// persistent HashMap extras, so a resize that does not rebuild extras via
    /// column reflow (rows-only / no-reflow) keeps on-screen non-BMP cells when
    /// [`invalidate_rings`](crate::extra_collection::CellExtras::invalidate_rings)
    /// clears the ring. Gated on the cell's COMPLEX flag so stale ring slots are
    /// not resurrected; skips cells already HashMap-backed (ZWJ/skin-tone).
    fn migrate_complex_ring_to_extras(&mut self) {
        // Only visible rows can match (`GridStorage::row` returns `None` past
        // `visible_rows`), so the ring-scrollback tail need not be probed.
        // `.min(rows.len())` guards a degenerate `visible_rows > rows.len()`
        // from aliasing through `row_index`'s `% rows.len()`.
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

    /// Copy every visible cell's ring-stored 24-bit fg/bg into the persistent
    /// extras map, so a resize that does not rebuild the extras by column reflow
    /// (same-size, rows-only, and every alternate-screen resize) keeps on-screen
    /// truecolor when `invalidate_rings` drops the RGB ring.
    ///
    /// The ring is created lazily by the first ring-stored RGB write after each
    /// `invalidate_rings`, so with no truecolor since the last resize (the
    /// second resize of a quiet flap) the early-out returns before reading a
    /// cell. Runs at the pre-`adjust_row_count` positions, so the entries ride
    /// the same row shift as combining marks and links.
    fn migrate_rgb_ring_to_extras(&mut self) {
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

    /// Trim (front then back) or grow the row buffer to match `target_rows`.
    /// Rows a shrink drains are staged into the lazy buffer when a store keeps
    /// them (`stages_evicted_rows`).
    ///
    /// Returns how many history lines a rows-grow revealed: ring-held lines
    /// directly above the old viewport that the caller's `visible_rows` update
    /// relabels as the top of the screen. The caller owes the cursor a downward
    /// shift by that count, then re-injects the returned `(viewport row,
    /// extras)` pairs into the live map (see `adjust_row_count_rows_only`).
    fn adjust_row_count(
        &mut self,
        target_rows: u16,
        new_cols: u16,
        policy: ResizePolicy,
    ) -> (usize, Vec<(u16, Box<ScrolledRowExtras>)>) {
        let target = target_rows as usize;
        let old_visible = usize::from(self.storage.visible_rows);

        // A rows-only resize must not shed ring history to fit the viewport:
        // reclassify viewport rows against ring history in place. Width
        // changes keep the migration below (their ring history rides
        // `take_scrollback_lines` / restore). `self.storage.cols` is still the
        // pre-resize width here.
        if new_cols == self.storage.cols {
            return self.adjust_row_count_rows_only(target, new_cols, policy);
        }

        if self.storage.rows.len() > target {
            // Bounded-cost counter (`tests/reflow/rows_only_cost_bound.rs`):
            // zero while the early return above stands; a rows-only resize
            // routed through this whole-ring migration would count history.
            #[cfg(any(test, debug_assertions))]
            if new_cols == self.storage.cols {
                super::count_rows_only_resize_migrated_rows(self.storage.rows.len() - target);
            }
            // Linearize ring buffer so pop/drain operate on logical order.
            self.storage.linearize_ring();
            let excess = self.storage.rows.len() - target;
            let scrollback = self
                .storage
                .total_lines
                .saturating_sub(self.storage.visible_rows as usize);
            let from_front = excess.min(scrollback);
            let from_back = excess - from_front;
            // Only the bottom drain changes the viewport; the front drain moves
            // history rows between tiers (`ResizeShape::pushed`).
            self.storage.last_resize_shape.pushed = row_u16(from_back);
            if from_front > 0 {
                // Stage front rows when a retaining store is attached (without
                // one `drain_lazy_buffer` would discard them) or detached for an
                // off-thread reflow (flushed on re-attach).
                if self.storage.stages_evicted_rows() {
                    let drained_rows: Vec<Row> = self.storage.rows.drain(..from_front).collect();
                    for row in drained_rows {
                        // These scrollback rows' extras were already extracted by
                        // scroll_up. `u16::MAX` as row_idx keeps HashMap-keyed
                        // lookups from attributing visible-row extras to them;
                        // ring lookups use the cell's own index.
                        let extracted =
                            Self::extract_row_extras(&row, &self.storage.extras, u16::MAX);
                        self.storage.lazy_buffer.push_row(&row, extracted);
                        // SAFETY: these rows came from storage.rows, and staging
                        // copied their content before returning their allocations.
                        unsafe { row.recycle(&mut self.storage.pages) };
                    }
                } else {
                    self.storage.recycle_rows(0..from_front);
                }
            }
            if from_back > 0 {
                // Push bottom visible rows to lazy scrollback before discarding,
                // so a height shrink does not lose them, wherever a store keeps
                // them or a detached reflow will flush them (as above).
                if self.storage.stages_evicted_rows() {
                    let start = self.storage.rows.len() - from_back;
                    // Visible rows pushed to scrollback by a height decrease;
                    // their extras are live in self.storage.extras keyed by
                    // visible row. After linearization and front-drain,
                    // scrollback occupies positions 0..remaining_scrollback, so
                    // Vec position p is visible row p - remaining_scrollback.
                    let remaining_scrollback = scrollback.saturating_sub(from_front);
                    let drained_rows: Vec<Row> = self.storage.rows.drain(start..).collect();
                    for (i, row) in drained_rows.into_iter().enumerate() {
                        let external_row = row_u16(start + i - remaining_scrollback);
                        let extracted =
                            Self::extract_row_extras(&row, &self.storage.extras, external_row);
                        self.storage.lazy_buffer.push_row(&row, extracted);
                        // SAFETY: the removed row belongs to storage.pages;
                        // the lazy buffer owns its content before this transfer.
                        unsafe { row.recycle(&mut self.storage.pages) };
                    }
                } else {
                    let end = self.storage.rows.len();
                    self.storage.recycle_rows(end - from_back..end);
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
        self.storage.last_resize_shape.revealed = row_u16(revealed);

        if target > self.storage.rows.len() {
            self.storage.linearize_ring();
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
            self.storage.last_resize_shape.appended = row_u16(rows_to_add);
        }
        // The width path's revealed rows carry no ring_extras hand-off: their
        // extras (if any) already rode the take/restore scrollback round trip.
        (revealed, Vec::new())
    }

    /// Rows-only [`adjust_row_count`](Self::adjust_row_count), for every grid
    /// shape: reclassify viewport rows against ring history in place, never
    /// shedding retention to fit the viewport (a rows-only resize changes no
    /// line's width or wrap topology, only where the live/history boundary
    /// falls). The retention cap is enforced like `scroll_up`'s at-capacity
    /// reuse, so surviving lines keep their absolute-row identity. O(|Δrows|),
    /// independent of ring depth. Returns the revealed-history count plus the
    /// revealed rows' extras for the caller to re-inject.
    ///
    /// Anchoring, so that shrink+grow is an identity:
    ///
    /// * Shrink trims trailing blank rows below the cursor, then demotes top
    ///   rows into the ring (the inverse of the grow reveal), and only when the
    ///   cursor sits too high for that pushes the remaining bottom rows into
    ///   history (content kept, reading order imperfect in that corner).
    /// * Grow reveals newest history at the top (absolute numbering intact, so
    ///   a scrolled-back reader's anchor stays exact).
    ///
    /// Through the ring the identity holds only while retention can hold the
    /// demoted rows; with `max_scrollback = 0` (the alternate screen) the
    /// resize undo (`grid::resize_undo`) keeps them until anything is drawn.
    /// Each arm's count is recorded in [`crate::ResizeShape`].
    fn adjust_row_count_rows_only(
        &mut self,
        target: usize,
        new_cols: u16,
        policy: ResizePolicy,
    ) -> (usize, Vec<(u16, Box<ScrolledRowExtras>)>) {
        let visible = self.storage.visible_rows as usize;
        debug_assert_eq!(
            self.storage.total_lines,
            self.storage.rows.len(),
            "rows-only resize: every ring row is a retained line"
        );

        if target < visible {
            let shrink = visible - target;
            self.storage.linearize_ring();

            // 1) Trim: trailing blank rows below the cursor are not content;
            // archived, a later grow would reveal them above real content.
            let trim = shrink.min(self.trailing_blank_rows_below_cursor());
            self.storage.last_resize_shape.trimmed = row_u16(trim);
            // A trimmed row is blank: the undo keeps only its place.
            if let Some(undo) = self.storage.resize_undo.as_mut() {
                undo.entries
                    .extend(std::iter::repeat_with(|| UndoRow::Trimmed).take(trim));
            }
            if trim > 0 {
                let keep = self.storage.total_lines - trim;
                self.storage.recycle_rows(keep..self.storage.rows.len());
                self.storage.total_lines = keep;
                // Their extras keys land >= `target` after the demote shift
                // and are swept by the caller's `retain_rows_below`. The
                // counter drops with the rows, so every surviving row keeps its
                // key.
                self.keep_keys_across_bottom_rows(BottomRows::Trimmed(trim));
            }
            let remaining = shrink - trim;

            // 2) Top-demote: the top `demote` viewport rows become the newest
            // history, a pure relabel (they already sit directly above the
            // surviving viewport in the linearized ring). Capped at the cursor
            // row so the cursor's line stays visible.
            let cursor_row = usize::from(self.storage.cursor.row).min(visible - 1);
            let demote = remaining.min(cursor_row);
            let hist = self.storage.total_lines - (visible - trim);
            if demote > 0 {
                // The demoted rows' live extras (keyed by visible row) move into
                // ring_extras in age order. Re-align the deque first: it may be
                // empty while history exists, and appending must not shift those
                // default entries.
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
            self.storage.last_resize_shape.demoted = row_u16(demote);
            self.storage.last_resize_shape.pushed = row_u16(bottom_push);
            // A pure top-demote moves every row by exactly `demote`, so the
            // selection is remappable (`TextSelection::adjust_for_rows_shrink`)
            // and only host-cached coordinates re-translate. The bottom-push
            // corner rotates bottom rows below existing history, a
            // non-monotonic map, so it invalidates the selection. The trim
            // moves nothing, so the shift is `demote`.
            self.storage.last_resize_row_shift = row_u16(demote);
            if bottom_push > 0 {
                self.force_selection_invalidation();
            } else if demote > 0 {
                self.invalidate_host_coordinates();
            }

            // 3) Bottom-push corner: the cursor sits too near the top (a full
            // screen with the cursor high, typical of a TUI). Push the bottom
            // rows as newest history; the demote kept the cursor's line.
            if bottom_push > 0 {
                let hist_after_demote = hist + demote;
                // The pushed rows are the bottom `bottom_push` of the surviving
                // viewport; their extras are keyed by post-demote visible row.
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
            }

            // 4) Retention cap: evict the oldest past a full ring, like
            // scroll_up's at-capacity eviction. Bounded by the height delta
            // (`total_lines <= visible + max_scrollback` on entry).
            let excess =
                (self.storage.total_lines - target).saturating_sub(self.storage.max_scrollback);
            if excess > 0 {
                // Bounded-cost counter: O(height delta), never O(history).
                #[cfg(any(test, debug_assertions))]
                super::count_rows_only_resize_migrated_rows(excess);
                // A tiered store (attached or detached for an off-thread
                // reflow) keeps retention past the ring, so evicted rows are
                // staged into the lazy buffer, as in `scroll_up`. Without one
                // the ring cap is the retention limit.
                if self.storage.stages_evicted_rows() {
                    for i in 0..excess {
                        let extras = self.storage.ring_extras.pop_front().flatten();
                        let storage = &mut self.storage;
                        storage.lazy_buffer.push_row_boxed(&storage.rows[i], extras);
                    }
                } else if self.storage.resize_undo.is_some() && excess == demote + bottom_push {
                    // Resize undo (`resize_undo`): a grid with no history had none
                    // before this shrink, so the evicted rows are exactly this
                    // call's demoted rows then pushed rows, in ring order. Keep them
                    // with their extras for a grow nothing drew in front of. Pushed
                    // rows are kept bottom-first, as a run of one-row shrinks would
                    // push them, so the LIFO hand-back appends them top-first.
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
                    // A stash that cannot hold this shrink's rows would hand
                    // back the wrong ones: drop it.
                    self.storage.resize_undo = None;
                    for _ in 0..excess {
                        self.storage.ring_extras.pop_front();
                    }
                }
                self.storage.recycle_rows(0..excess);
                self.storage.total_lines -= excess;
            }
        } else if target > visible {
            if policy == ResizePolicy::ConPty {
                // ConPTY grow: append the whole delta as blank rows at the
                // bottom and reveal nothing. conhost paints the grown rows
                // `CSI K` (see `ResizePolicy`), so a line the Native reveal
                // moved into the viewport would be painted over and lost. No
                // row changes tier, so `ring_extras` are untouched; the counter
                // moves with the appended rows, so no key slides.
                self.storage.linearize_ring();
                let rows_to_add = target - visible;
                let rows = &mut self.storage.rows;
                let pages = &mut self.storage.pages;
                for _ in 0..rows_to_add {
                    // SAFETY: New rows are stored in the same `GridStorage`
                    // that owns `pages`, and rows drop before the backing pages.
                    rows.push(unsafe { Row::new(new_cols, pages) });
                }
                self.storage.total_lines += rows_to_add;
                self.storage.last_resize_shape.appended = row_u16(rows_to_add);
                self.keep_keys_across_bottom_rows(BottomRows::Appended(rows_to_add));
                return (0, Vec::new());
            }
            // Grow: reveal up to (target - visible) newest history lines by pure
            // reclassification: they already sit directly above the viewport,
            // so the caller's visible_rows update relabels them with absolute
            // numbering intact. Their `ring_extras` go back to the caller for
            // re-injection after it shifts the old viewport's extras down;
            // injected here, they would ride that shift. First the resize undo
            // hands back, newest first, the rows the shrinks took: demoted rows
            // above the viewport for the reveal, pushed rows and trimmed blanks
            // top-first for the bottom append.
            let restored_bottom = self.take_undo_rows(target - visible, new_cols);
            let hist = self.storage.total_lines - visible;
            let revealed = (target - visible).min(hist);
            self.storage.last_resize_shape.revealed = row_u16(revealed);
            let keep = hist - revealed;
            let mut reveal_extras: Vec<(u16, Box<ScrolledRowExtras>)> = Vec::new();
            // Deque tail = newest ring row = the bottom revealed viewport row
            // (`revealed - 1`); the deque may hold fewer entries than history
            // rows (older rows without extras), which `keep` tolerates.
            for j in (0..revealed).rev() {
                if self.storage.ring_extras.len() <= keep {
                    break;
                }
                if let Some(bx) = self.storage.ring_extras.pop_back().flatten() {
                    reveal_extras.push((row_u16(j), bx));
                }
            }
            // Pushed rows come back at the bottom, where the shrink took them,
            // with extras at their final rows (injected by the caller after its
            // reveal shift); trimmed blanks come back in place among them.
            let mut blanks_restored = 0;
            if !restored_bottom.is_empty() {
                // Appended in order after the viewport, as the blank append
                // below does: the ring must start at its physical row 0.
                self.storage.linearize_ring();
                let base = revealed + visible;
                let n = restored_bottom.len();
                let mut pushed = 0;
                let rows = &mut self.storage.rows;
                let pages = &mut self.storage.pages;
                for (i, entry) in restored_bottom.into_iter().enumerate() {
                    let Some((row, extras)) = entry else {
                        // SAFETY: New rows are stored in the same `GridStorage`
                        // that owns `pages`, and rows drop before the backing
                        // pages.
                        rows.push(unsafe { Row::new(new_cols, pages) });
                        blanks_restored += 1;
                        continue;
                    };
                    // SAFETY: the row is stored in the same `GridStorage` that
                    // owns `pages`, and rows drop before the backing pages.
                    rows.push(unsafe { Row::from_snapshot(&row, new_cols, pages) });
                    pushed += 1;
                    if let Some(bx) = extras {
                        reveal_extras.push((row_u16(base + i), bx));
                    }
                }
                self.storage.total_lines += n;
                self.storage.last_resize_shape.restored_bottom = row_u16(pushed);
                self.storage.last_resize_shape.appended = row_u16(blanks_restored);
                // A trimmed blank comes back with the counter rising (the trim
                // lowered it), so no key moves. A pushed row comes back under a
                // fixed counter, the inverse of the shrink's eviction, which
                // renumbers the rows the push renumbered.
                self.keep_keys_across_bottom_rows(BottomRows::Appended(blanks_restored));
                if pushed > 0 {
                    self.note_bottom_end_renumbered();
                }
            }
            // Any remaining growth needs fresh blank rows at the bottom.
            if target > self.storage.total_lines {
                self.storage.linearize_ring();
                let rows_to_add = target - self.storage.total_lines;
                let rows = &mut self.storage.rows;
                let pages = &mut self.storage.pages;
                for _ in 0..rows_to_add {
                    // SAFETY: New rows are stored in the same `GridStorage`
                    // that owns `pages`, and rows drop before the backing pages.
                    rows.push(unsafe { Row::new(new_cols, pages) });
                }
                self.storage.total_lines += rows_to_add;
                self.storage.last_resize_shape.appended = row_u16(blanks_restored + rows_to_add);
                // As under ConPTY: the reveal above is a pure relabel, and
                // these rows take fresh keys above every key in use, so the
                // counter rises with them (`keep_keys_across_bottom_rows`).
                self.keep_keys_across_bottom_rows(BottomRows::Appended(rows_to_add));
            }
            return (revealed, reveal_extras);
        }
        // target == visible: nothing to reclassify.
        (0, Vec::new())
    }

    /// Record that a rows-only resize renumbered retained rows: the resize undo
    /// handing a pushed row back to the bottom of a grid that keeps no history
    /// changes the retained total under a fixed counter, the exact inverse of
    /// the shrink whose eviction pushed it.
    ///
    /// `oldest_absolute_row()` is `counter − retained_total`. Moving rows across
    /// the live/history boundary (reveal, top-demote) keeps the total fixed; the
    /// trim and the blank append move the counter with their rows
    /// ([`Self::keep_keys_across_bottom_rows`]). The epoch flags a renumbering
    /// the `(content_gen, absolute_row_revision, geometry)` keys cannot see, as
    /// in the Kitty unscroll and the scrollback rewrap.
    fn note_bottom_end_renumbered(&mut self) {
        self.storage.history_renumber_epoch = self.storage.history_renumber_epoch.saturating_add(1);
    }

    /// Move `absolute_row_counter` with the rows a rows-only resize appended at
    /// or trimmed from the bottom, under both policies, so `oldest_absolute_row()`
    /// (`counter − retained_total`) stays put and every retained row keeps its
    /// key. Nothing is renumbered, so the epoch does not move, and the counter
    /// never falls below the retained total, so `oldest_absolute_row()` always
    /// names the same row `visible_to_absolute` does: a shell's OSC 133 marks
    /// are recorded by one and read back by the other.
    ///
    /// Appended rows take fresh keys above every key in use. Trimmed rows were
    /// blank rows below the cursor, and their keys go to the rows that next
    /// appear there, as in conhost's buffer. Those keys named live rows only,
    /// and the history-row memo in `visible_row_view` keys on `content_gen`,
    /// which every resize bumps, so a reused key cannot hit a stale entry.
    ///
    /// A reader aim held across an off-thread reflow reads the counter's rise
    /// as lines scrolled into history, so its `at_counter` moves with it.
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

    /// The inverse of [`Self::extract_row_extras`]: re-attach a revealed ring
    /// row's extracted extras to its viewport row in the live map. Demote and
    /// reveal are pure relabels, so the cells' `is_complex`/RGB-overflow markers
    /// still point here. The cells claim their entries again (`cell_extra_mut`
    /// sets HAS_EXTRAS): the row may have lost the bit on its way into history,
    /// and an unclaimed mark is neither drawn nor copied.
    fn inject_scrolled_extras(&mut self, row_idx: u16, e: &ScrolledRowExtras) {
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
        // The two channels the extractor carries without a per-column map, as the
        // history readers restore them (`restore_image_spans`,
        // `mirror_wide_spacer_extras`): the image spans, and the spacers' inherited
        // values. An image goes through the map's own entry point, which re-arms the
        // scroll-off image scan (an ED 2 while the row was in history disarmed it).
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

    /// Reflow lines when column count changes.
    fn reflow_columns(
        &mut self,
        target_rows: u16,
        new_cols: u16,
        cursor_row: usize,
        cursor_col: u16,
        policy: ResizePolicy,
    ) {
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
        );
    }

    /// Pad or truncate to target row count and update grid state after reflow.
    ///
    /// When the rewrap produced more rows than `target_rows`, excess top rows
    /// are staged in the lazy buffer: Native pushes only what brings the
    /// cursor on screen, ConPTY what conhost demotes
    /// ([`Self::conpty_reflow_demote`]). They are the newest history, and
    /// `restore_reflowed_scrollback`, which every width reflow runs after this,
    /// seats them once the older history is back.
    fn finalize_reflow(
        &mut self,
        target_rows: u16,
        mut result: ReflowResult,
        new_cols: u16,
        policy: ResizePolicy,
    ) {
        let target_rows = usize::from(target_rows);

        let push_count = if policy == ResizePolicy::ConPty {
            Self::conpty_reflow_demote(&result, target_rows)
        } else if result.rows.len() > target_rows && result.cursor_row >= target_rows {
            let rows_to_push = result.rows.len() - target_rows;
            rows_to_push.min(result.cursor_row + 1 - target_rows)
        } else {
            0
        };
        if push_count > 0 {
            // Collect drained rows so we can borrow result.extras
            // for extract_row_extras while iterating.
            let drained_rows: Vec<Row> = result.rows.drain(..push_count).collect();
            for (i, row) in drained_rows.into_iter().enumerate() {
                let row_idx = u16::try_from(i).unwrap_or(u16::MAX);
                let extracted = Self::extract_row_extras(&row, &result.extras, row_idx);
                self.storage.lazy_buffer.push_row(&row, extracted);
                // SAFETY: reflow built these rows in result.pages, not the old
                // storage.pages. The lazy buffer now owns the preserved data.
                unsafe { row.recycle(&mut result.pages) };
            }

            // Shift extras row indices to match the row removal.
            if let Ok(n) = u16::try_from(push_count) {
                result.extras.shift_rows_up_by(0, n);
            }
            result.cursor_row -= push_count;
            // The saved cursor rides the push too; one whose row went to
            // history is left on the top row, as a rows-only demote leaves it.
            result.saved = result
                .saved
                .map(|(row, col)| (row.saturating_sub(push_count), col));
        }

        let kept = target_rows.min(result.rows.len());
        for row in result.rows.drain(kept..) {
            // SAFETY: each removed reflow row belongs to the new result arena.
            unsafe { row.recycle(&mut result.pages) };
        }

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

        // Rescan any_double_width: double-width rows may have been pushed to
        // scrollback, and a stale flag would slow every cursor operation.
        self.storage.any_double_width = self.storage.rows.iter().any(|r| {
            matches!(
                r.line_size(),
                LineSize::DoubleWidth | LineSize::DoubleHeightTop | LineSize::DoubleHeightBottom
            )
        });

        let max_row = row_u16(self.storage.rows.len().saturating_sub(1));
        self.storage.cursor.row = row_u16(result.cursor_row).min(max_row);
        self.storage.cursor.col = result.cursor_col.min(new_cols.saturating_sub(1));
        // The saved cursor's row is not clamped: one whose row fell off the
        // bottom (rows past the target below the cursor are dropped) stays
        // past the grid, as a trimmed row's slot does (`resize_viewport_state`).
        if let Some((row, col)) = result.saved {
            self.storage.saved_cursor.cursor.row = row_u16(row);
            self.storage.saved_cursor.cursor.col = col.min(new_cols.saturating_sub(1));
        }
    }

    /// How many top rows of a rewrapped viewport conhost demotes to its
    /// scrollback when a ConPTY width change leaves more rows than fit: the
    /// rows carrying content (through the cursor's row and the last non-blank
    /// row) minus the target height, at most the rows above the cursor; the
    /// rows-only shrink's trim + top-demote applied to the rewrapped rows.
    ///
    /// The Native count (only what brings the cursor on screen) loses a line
    /// under conhost whenever a non-blank row sits below the cursor (PSReadLine's
    /// `>>`): conhost's row-0 repaint overwrites the row Native left at the top.
    /// Relies on `result.cursor_row` keeping the cursor's row through the
    /// cursor's cell (`cursor_row_copy_len`).
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

    /// Rewrap the visible rows to a new column count, in either direction.
    ///
    /// Soft-wrapped continuation runs are merged into their logical line and
    /// re-chunked at the new width, so rows come out canonical (`24,24,…`, not
    /// ragged `24,6,24,6,…`). Rows are read in place from the ring through one
    /// reusable merge buffer. The live and saved cursors ride the rewrap to the
    /// cells their text lands on (`RewrapCursors`).
    fn reflow_rewrap_columns(
        &mut self,
        target_rows: u16,
        new_cols: u16,
        cursor_row: usize,
        cursor_col: u16,
        old_extras: Option<&CellExtras>,
        policy: ResizePolicy,
    ) {
        let mut new_pages = PageStore::new();
        let visible_count = usize::from(self.storage.visible_rows);
        let mut new_rows: Vec<Row> = Vec::with_capacity(visible_count);
        // A saved cursor names a cell only while it is on a row the grid has;
        // past the grid (a shrink left it there) it names no text and stays.
        let saved = self.storage.saved_cursor;
        let saved_src = (saved.valid && usize::from(saved.cursor.row) < visible_count)
            .then(|| (usize::from(saved.cursor.row), saved.cursor.col));
        let mut cursors = RewrapCursors {
            src: [Some((cursor_row, cursor_col)), saved_src],
            dst: [None, None],
        };
        let mut merge_buf: Vec<super::Cell> = Vec::with_capacity(self.storage.cols as usize);
        let mut merge_coords: Vec<CellCoord> = Vec::new();
        let mut new_extras = CellExtras::new();
        let keep_cursor_cell = policy == ResizePolicy::ConPty;

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
            // The cells a line with no continuation contributes: its content,
            // and under ConPTY for the cursor's row every cell through the
            // cursor's (`cursor_row_copy_len`). The merge path applies the same
            // rule to the line's last row.
            let copy_len = if keep_cursor_cell && cursor_row == first_row_idx {
                cursor_row_copy_len(row, cursor_col)
            } else {
                row.len() as usize
            };
            let has_cont = i + 1 < visible_count
                && self
                    .row(row_u16(i + 1))
                    .is_some_and(is_mergeable_continuation);

            let source_line_size = row.line_size();

            // DECDWL/DECDHL rows are not reflowed; resize them in place.
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
                    keep_cursor_cell,
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
                // SAFETY: `new_row` is appended to `new_rows` and returned
                // alongside `new_pages` in the same reflow result.
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
                    // Single source row `i` chunked across new rows → compute coords.
                    source: if old_extras.is_some() {
                        ExtrasSource::Row(row_u16(i))
                    } else {
                        ExtrasSource::None
                    },
                    old_extras,
                    new_extras: &mut new_extras,
                };
                // Content is the row's own length: ConPTY's cursor-row cells
                // past it are copied, not counted.
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
                // Inherit the original row's wrapped flag on the first chunk, as
                // the shrink path does, so a continuation of a scrollback line
                // keeps its flag (cross-boundary search/copy).
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
            },
            new_cols,
            policy,
        );
    }

    /// Merge continuation rows into `merge_buf`, then chunk into new rows.
    ///
    /// Advances `*i` past all continuation rows consumed.
    ///
    /// A row the next one continues was filled to its last column by autowrap,
    /// so all its cells are content, trailing blanks included (padded to the
    /// full old width, or a width sweep erodes one space per chunk boundary),
    /// except a recorded early-wrap hole (`Row::continued_len`): that one cell
    /// is dropped, also when the wrap blanked it with a background colour, and
    /// a space or glyph really written in the last column is kept.
    #[allow(clippy::too_many_arguments)]
    fn merge_continuation_rows(
        &self,
        start: usize,
        visible_count: usize,
        cursor_row: usize,
        cursor_col: u16,
        keep_cursor_cell: bool,
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
        let mut offsets: [Option<usize>; 2] = [None, None];
        // Where the line's content ends in `merge_buf`: after the last source
        // row with content. An inner row's pad counts once later content
        // follows it; the last row's trailing blanks and ConPTY's cursor-cell
        // extension do not.
        let mut content_end = 0usize;

        // Save the first row's wrapped flag and line_size before merging: a
        // continuation of a scrollback line must keep its flag for
        // cross-boundary search/copy. line_size (DECDWL/DECDHL) comes from the
        // first row, since continuations are always single-width.
        let first_row_was_wrapped = self.row(row_u16(start)).is_some_and(Row::is_wrapped);
        let first_row_line_size = self
            .row(row_u16(start))
            .map_or(LineSize::SingleWidth, Row::line_size);

        // Copy first row's cells.
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

        // Copy continuation rows.
        while *i + 1 < visible_count
            && self
                .row(row_u16(*i + 1))
                .is_some_and(is_mergeable_continuation)
        {
            *i += 1;
            // Each merged continuation row is O(cols) work; count it so the
            // `reflow_linear_time*` cost check sees per-row cost even when the
            // screen is one logical line.
            #[cfg(any(test, debug_assertions))]
            super::count_reflow_row_op();
            // The row just appended is continued by this one: it contributes
            // its full old width less a recorded hole. Pad up to that, or cut
            // a hole its length covers (a hole blanked with a background).
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

        // ConPTY: a cursor on the line's last row keeps every cell through its
        // own (`cursor_row_copy_len`); on an earlier row the autowrap pad
        // already reaches past the cursor.
        if keep_cursor_cell
            && cursor_row == row_idx
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
            // SAFETY: `new_row` is appended to `new_rows` and returned
            // alongside `new_pages` in the same reflow result.
            let mut new_row = unsafe { Row::new(new_cols, new_pages) };
            new_row.set_line_size(first_row_line_size);
            for (k, offset) in offsets.into_iter().enumerate() {
                if offset.is_some()
                    && let Some((_, col)) = cursors.src[k]
                {
                    cursors.dst[k] = Some((new_rows.len(), col.min(new_cols.saturating_sub(1))));
                }
            }
            new_rows.push(new_row);
        } else {
            let first_idx = new_rows.len();
            let mut points = RewrapCursors::points(offsets);
            let mut extras_ctx = ExtrasCopyCtx {
                // Cells span several source rows: index the coord table.
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
            // Inherit the first merge row's wrapped flag and line_size on the
            // first output chunk, as the other paths do.
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

    // Resize dimension bounds (the ingress clamp).

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

    // Grid-level reflow: narrower -> wider -> same width.

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

    // ResizePolicy::ConPty: the seam accounting conhost's repaint requires.

    /// Ten `L<i>` lines on a 4-row grid with history: `L0..L6` in the ring
    /// history, `L7 L8 L9 <blank>` on screen, cursor on the blank row.
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

    /// A ConPTY rows-grow appends blank rows at the bottom: history, viewport
    /// rows and cursor row stay put, no row shift is published, and the counter
    /// moves with the appended rows, so no key moves and nothing is renumbered.
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

    /// A ConPTY height drag down and back: the grow appends four blank rows,
    /// the shrink trims exactly those, and every key — history and viewport
    /// alike — is the same at each step, with no renumbering in between.
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

    /// A reader who scrolled up past the attached rows while the history was
    /// out for an off-thread rewrap lands where they asked, though a ConPTY
    /// grow moved the counter meanwhile: the aim counts the counter's rise as
    /// lines scrolled into history, and a grow scrolls none.
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

    /// Contrast: the Native grow on the same grid reveals the four newest
    /// history lines at the top, the move conhost's repaint paints over.
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

    /// A 15-char logical line split 10+5 across the history/viewport seam
    /// (head `ABCDEFGHIJ` is the one history line, tail `KLMNO` is the wrapped
    /// row 0), then `x`, `y`; cursor after `y`.
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

    /// A ConPTY widen neither lifts the boundary continuation into the history
    /// rewrap nor fills the viewport from history: the tail stays at row 0 as a
    /// standalone fragment and the head stays the one history line.
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

    /// A ConPTY width round trip leaves the history exactly as it was: the
    /// head re-enters every later rewrap at the 10 chars it holds. Without the
    /// sever, the still-flagged fragment makes the next take read the head's
    /// trailing blanks as content, and 20→10 adds a blank history line.
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

    /// Contrast: the Native widen rejoins the line in history and pulls it
    /// back into the viewport (bottom anchoring), which is right when the grid
    /// is the frame and exactly what conhost paints over.
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

    /// Rows-shrink is policy-independent: conhost's shrink repaint (trailing
    /// blanks dropped, else a top-demote) matches the Native shape.
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

    /// `rows` rows labelled `R0`, `R1`, … (every row non-blank), cursor parked on
    /// `cursor_row`, ring retention `max_scrollback` (0 is the alternate
    /// screen's shape).
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

    /// `ResizeShape` names each arm a resize ran, with its count: trim,
    /// top-demote, bottom-push, the grow's reveal and blank append, and the
    /// width rewrap; with retention 0, the resize undo's stash and hand-back.
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

        // Trim: two content rows, cursor on the second, four blank rows below.
        let mut grid = labelled_screen(2, 1, 100);
        grid.resize(6, 10);
        assert_eq!(grid.take_last_resize_shape(), shape(0, 0, 0, 0, 4));
        grid.resize(4, 10);
        assert_eq!(grid.last_resize_shape(), shape(2, 0, 0, 0, 0), "peek");
        assert_eq!(grid.take_last_resize_shape(), shape(2, 0, 0, 0, 0));
        assert_eq!(grid.last_resize_shape(), ResizeShape::default(), "drained");
        assert_eq!(grid.take_last_resize_row_shift(), 0, "a trim moves nothing");
        assert_eq!(grid.row(0).unwrap().to_string(), "R0");

        // Trim, then demote for the rest: one blank row below the cursor.
        let mut grid = labelled_screen(6, 5, 100);
        grid.erase_line();
        grid.set_cursor(4, 0);
        grid.resize(3, 10);
        assert_eq!(grid.take_last_resize_shape(), shape(1, 2, 0, 0, 0));
        assert_eq!(grid.row(0).unwrap().to_string(), "R2");
        assert_eq!(grid.cursor_row(), 2);

        // Demote on a retaining grid, and the grow reveals it back: identity.
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

        // The same flap with no retention: the demote is evicted from the ring
        // but kept by the resize undo, and the grow hands it back.
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

        // A write between the halves drops the undo: the grow appends, and
        // every row stays two rows higher than it was painted.
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

        // Bottom-push corner: a full screen with the cursor on row 0.
        let mut grid = labelled_screen(6, 0, 100);
        grid.resize(4, 10);
        assert_eq!(grid.take_last_resize_shape(), shape(0, 0, 2, 0, 0));
        assert_eq!(grid.row(0).unwrap().to_string(), "R0", "the top stays put");

        // Both: the cursor on row 1 caps the demote at 1, the corner pushes 1.
        let mut grid = labelled_screen(6, 1, 100);
        grid.resize(4, 10);
        assert_eq!(grid.take_last_resize_shape(), shape(0, 1, 1, 0, 0));

        // A same-size resize records an empty shape, not the previous one.
        grid.resize(4, 10);
        grid.resize(4, 10);
        assert_eq!(grid.take_last_resize_shape(), ResizeShape::default());

        // Width: the rewrap is flagged; the no-reflow path (the alt screen's)
        // is not.
        let mut grid = labelled_screen(6, 4, 100);
        grid.resize(6, 12);
        assert!(grid.take_last_resize_shape().reflowed);
        grid.resize_no_reflow(6, 10);
        assert!(!grid.take_last_resize_shape().reflowed);

        // The ConPTY grow appends; the Native grow on the same grid reveals.
        let mut conpty = ten_lines_in_four_rows();
        conpty.resize_with_policy(8, 10, ResizePolicy::ConPty);
        assert_eq!(conpty.take_last_resize_shape(), shape(0, 0, 0, 0, 4));
        let mut native = ten_lines_in_four_rows();
        native.resize(8, 10);
        assert_eq!(native.take_last_resize_shape(), shape(0, 0, 0, 4, 0));
        native.assert_invariants();
        conpty.assert_invariants();
    }

    /// PSReadLine's shape in small: `L0..=L5` in history, `L6..=L9` on rows
    /// 0..4, a prompt `>` on row 4 with the cursor after it, and a non-blank
    /// `>>` on row 5 below the cursor.
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

    /// A ConPTY width change that leaves more rows than fit demotes the whole
    /// excess from the top, as conhost does, and keeps the non-blank row under
    /// the cursor; Native pushes only the one row that brings the cursor on
    /// screen and cuts `>>` — the push conhost's row-0 repaint then overwrites.
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
            lines(0..=6),
            "Native: the cursor's minimum"
        );
        assert_eq!(screen_text(&native), ["L7", "L8", "L9", ">"]);
        assert_eq!(native.cursor_row(), 3);
    }

    /// The demote is capped at the rows above the cursor (the cursor's row is
    /// never demoted), and a blank row under the cursor is dropped rather than
    /// counted — both exactly the rows-only shrink's trim and demote.
    #[test]
    fn conpty_width_shrink_demotes_no_further_than_the_cursor_and_drops_blanks() {
        // Cursor on row 1 with four non-blank rows below it: 6 content rows
        // into 3 demotes only the one row above the cursor.
        let mut high = prompt_above_a_non_blank_row();
        high.move_cursor_to(1, 0);
        high.resize_with_policy(3, 8, ResizePolicy::ConPty);
        assert_eq!(high.scrollback_lines(), 7);
        assert_eq!(high.row(0).unwrap().to_string(), "L7");
        assert_eq!(high.cursor_row(), 0);
        high.assert_invariants();

        // The `>>` row erased: the blank row under the cursor goes first, so
        // the policies agree again (one row demoted, the cursor on the bottom).
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

    /// pwsh's prompt in small: `L0..=L5` in history, `L6..=L9` on rows 0..=3,
    /// and a 15-cell prompt autowrapped at 10 columns (`P` x10 on row 4, `Q` x5
    /// on row 5) with the cursor one column past its last glyph, as pwsh's
    /// `CSI 1 C` leaves it: offset 16.
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

    /// At a width that divides the prompt exactly (15 into 5) the cursor's
    /// cell starts a row of its own under ConPTY, as conhost's Reflow puts it
    /// (`cursor_row_copy_len`), and that row counts toward the demote: 8
    /// content rows into 6 demotes two. Native clamps the cursor to the last
    /// glyph, counts 7 rows and pushes one.
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

        // Widening back rejoins the cursor's row with the prompt: the cursor
        // is back on its column, one past the last glyph.
        conpty.resize_with_policy(6, 10, ResizePolicy::ConPty);
        assert_eq!(history_text(&conpty), lines(0..=7));
        assert_eq!(
            screen_text(&conpty),
            ["L8", "L9", "PPPPPPPPPP", "QQQQQ", "", ""]
        );
        assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (3, 6));
        conpty.assert_invariants();

        let mut native = prompt_with_the_cursor_past_its_end();
        native.resize(6, 5);
        assert_eq!(history_text(&native), lines(0..=6), "Native pushes one");
        assert_eq!(
            screen_text(&native),
            ["L7", "L8", "L9", "PPPPP", "PPPPP", "QQQQQ"]
        );
        assert_eq!((native.cursor_row(), native.cursor_col()), (5, 4));
    }

    /// The same rule on a line with no continuation: the cursor keeps its
    /// offset under ConPTY instead of being clamped to the last glyph, so a
    /// width that divides the cells through the cursor's gives the cursor a
    /// row of its own, and one that does not moves only the cursor's column.
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
        // 6 glyphs, the cursor at 8: nine cells into 4 columns is three rows.
        let mut conpty = prompt();
        conpty.resize_with_policy(3, 4, ResizePolicy::ConPty);
        assert_eq!(screen_text(&conpty), ["PPPP", "PP", ""]);
        assert!(conpty.row(2).unwrap().is_wrapped());
        assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (2, 0));
        conpty.assert_invariants();
        let mut native = prompt();
        native.resize(3, 4);
        assert_eq!(screen_text(&native), ["PPPP", "PP", ""]);
        assert!(!native.row(2).unwrap().is_wrapped());
        assert_eq!((native.cursor_row(), native.cursor_col()), (1, 2));

        // Into 5 columns nine cells is two rows either way; only the cursor's
        // column differs.
        let mut conpty = prompt();
        conpty.resize_with_policy(3, 5, ResizePolicy::ConPty);
        assert_eq!(screen_text(&conpty), ["PPPPP", "P", ""]);
        assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (1, 3));
        conpty.assert_invariants();
        let mut native = prompt();
        native.resize(3, 5);
        assert_eq!((native.cursor_row(), native.cursor_col()), (1, 1));
    }

    /// The rule holds on a row with no text: a cursor parked at column 70 of
    /// an empty row keeps its offset under ConPTY (conhost's repaint places it
    /// at `CSI 7;11 H` after 80→60, `CSI 7;31 H` after 60→40, `CSI 6;71 H`
    /// after 40→80). Native keeps it on its row, clamped.
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
        assert_eq!((native.cursor_row(), native.cursor_col()), (5, 59));
    }
}
