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

/// WHO repaints the viewport after a resize — the fact the row accounting at
/// the history/viewport seam has to agree with, or a line is duplicated on one
/// side of the seam or lost on the other.
///
/// Under a Unix PTY nobody repaints: the grid IS the frame, so a resize may
/// reclassify rows across the seam however it likes and the reader sees
/// exactly that. Under Windows ConPTY the grid is NOT the frame: conhost keeps
/// its own buffer and, after every `ResizePseudoConsole`, repaints the WHOLE
/// viewport from it — measured on Windows 11 26200 with `cmd.exe` under a
/// bare `CreatePseudoConsole`, the probe resizing with `ResizePseudoConsole`
/// and capturing the output pipe byte for byte (2026-09-22 and 2026-09-27):
///
/// * every resize emits `CSI ?25l`, `CSI H`, then the `visible_rows` rows top
///   to bottom, each as `<text> CSI K CR LF` (no `CSI K` on a full row, no
///   CR LF after the last), except that a logical line spanning several rows
///   goes out as ONE run that wraps by autowrap; then a CUP to conhost's
///   cursor, left out when the last row's text already leaves the cursor
///   there — the viewport is rewritten top to bottom, anchored at ROW 0;
/// * rows-grow 24→40 (after 60 numbered lines): row 0 stays the old row 0
///   (`grow 40`), the 16 grown rows are painted `CSI K` — conhost reveals
///   NOTHING from its scrollback, the cursor row is unchanged (`CSI 24;31 H`);
///   a grow that follows a shrink (16→30) likewise repaints the post-shrink
///   top row at row 0 and blanks below it;
/// * rows-shrink 24→16 on a full screen: row 0 becomes old row 8 — the top 8
///   rows leave for conhost's scrollback (a top-demote); a shrink whose bottom
///   rows are blank just drops them; with the cursor on row 3 and content
///   below it (the bottom-push corner), row 0 becomes the cursor's row, rows
///   1..15 are old rows 4..18 and the cursor lands on row 0 — conhost demotes
///   exactly the rows above the cursor and cuts the rest off the BOTTOM, and
///   the grow back to 24 paints the 8 grown rows `CSI K`, so the cut rows are
///   never shown again. The Native shrink (trim the trailing blank, demote up
///   to the cursor row, bottom-push the remainder into history) leaves the
///   identical viewport and cursor, and keeps the cut rows in history instead
///   of dropping them (below the demoted rows, out of reading order, as that
///   corner always has). A width change that leaves more rows than fit
///   demotes by the same rule, counted on the rewrapped rows — re-measured
///   2026-09-27 with pwsh, PSReadLine's `>>` on the row under the cursor:
///   24x80→20x60 demoted all four rows, where the Native width shrink pushes
///   only the three that bring the cursor on screen
///   (`Grid::conpty_reflow_demote`). The rewrapped rows are counted with the
///   cursor's row kept through the cursor's cell, as conhost's Reflow keeps it
///   (`cursor_row_copy_len`): pwsh parks the cursor one column past the
///   prompt's last glyph (offset 117 after its 116 columns), and at a width
///   whose rows end right before that cell (58, 39 and 29) it is a row of its
///   own — conhost demoted one row more there, with or without `>>` under the
///   prompt;
/// * widen 80→120: the viewport's own lines unwrap in place, row 0 stays row
///   0, the freed rows at the bottom are painted `CSI K` — no history is
///   pulled in to fill them; a wrap CONTINUATION sitting at row 0 (head in
///   history) is repainted at row 0 as a standalone 70-char fragment, never
///   rejoined with its head; narrowing it to 60 splits the fragment 60+10 and
///   scrolls the viewport to keep the cursor on screen (row 0 = the 10-char
///   tail), and widening back to 80 does NOT rejoin the 60+10 split.
///   Re-measured 2026-09-27 with pwsh at 23x80 (`aterm ctl cast`, 40 lines
///   of 102 columns, the 30th cut at the seam): the 80→120 repaint is
///   `CSI H`, the 22-column tail `CSI K`, then every later line on one row —
///   the tail is never rejoined, so the history keeps the 80-column head
///   and the viewport keeps the tail conhost paints;
/// * the rewrap trusts continuation flags that conhost's OUTPUT sets and
///   breaks: a wrapped line that starts on the bottom row goes out as its
///   first 80 columns, CR LF, then `CSI <rows-1>;80 H` re-writing the 80th
///   character so the rest autowraps; pwsh's `cls` is `CSI K` down every
///   row, which must break each row's link to the row below
///   (`Grid::clear_wrap_into_next_row`) or a later line written there is
///   rewrapped into the line above it on the next width change.
///
/// So the seam-crossing moves the Native policy makes for the reader's
/// benefit — revealing history into grown rows, lifting the boundary
/// continuation into the history rewrap, pulling history back to bottom-anchor
/// a widened viewport — are exactly the moves conhost's repaint then paints
/// over: revealed rows are overwritten (the audit's 16 lost lines on a 24→40
/// grow), a filled tail is painted twice (the audit's duplicated wrap
/// fragment). The `ConPty` policy makes none of them, and on a width change
/// it severs the row-0 fragment from its head, as conhost does
/// (`sever_top_row_continuation`).
///
/// Absolute row keys: the ConPTY grow appends its rows at the BOTTOM and the
/// shrink's trim drops blank rows there, so under this policy both move
/// `absolute_row_counter` with those rows (`keep_keys_across_bottom_rows`) and
/// every retained row keeps its key. Under a conhost host every height drag is
/// such a grow or trim, and the Native bookkeeping for the same two arms
/// (counter fixed, keys slide, `history_renumber_epoch` bumped) would move every
/// OSC 133 command mark, output block and annotation by the height delta on
/// each step — measured before this: in a session whose retention had
/// evicted history, a 24→40 grow left a completed prompt mark's row reading
/// `""` instead of its prompt, 16 rows off. (With nothing evicted,
/// `oldest_absolute_row()` saturates at 0 and hides the slide.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ResizePolicy {
    /// The grid is the frame (Unix PTYs, replays, tests): a rows-grow REVEALS
    /// retained history at the top, a width change bottom-anchors the viewport
    /// (boundary-continuation lift + deficit fill). Byte-identical to the
    /// behaviour before this enum existed.
    #[default]
    Native,
    /// conhost repaints the viewport after every resize, row-0-anchored, from
    /// its own buffer (the measurements above): a rows-grow appends fresh
    /// blank rows at the BOTTOM and reveals nothing; a width change rewraps
    /// the viewport in place and the off-screen history separately, with no
    /// continuation lift and no deficit fill, a continuation at row 0 becomes
    /// a line of its own, and the cursor's row keeps every cell through the
    /// cursor's, so the cursor keeps its offset in its line where Native
    /// clamps it to the last glyph. A rows-only shrink keeps the Native shape
    /// (trim trailing blanks, top-demote, bottom-push corner), which leaves
    /// the viewport conhost paints; a shrink that rewraps demotes that same
    /// count from the rewrapped rows, where Native pushes only what brings the
    /// cursor on screen. The grow's append and the shrink's trim move
    /// `absolute_row_counter` with their rows, so no key moves (above).
    ConPty,
}

/// Rows a ConPTY rows-only resize added at, or dropped from, the BOTTOM of
/// the buffer: what `Grid::keep_keys_across_bottom_rows` moves the counter by.
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

/// How many cells of the cursor's row a ConPTY rewrap copies: its content and
/// every cell through the cursor's, blank or not. conhost's Reflow copies the
/// cursor's row through `max(MeasureRight, cursor.x + 1)`, so a cursor parked
/// past the row's last glyph keeps its offset in the logical line, and when the
/// cells before it end on a row boundary the cursor's cell starts a row of its
/// own; on a row with no text a cursor past the new width moves down with its
/// cells. Measured 2026-09-27 (pwsh 7.6, `aterm ctl cast`): the 116-column
/// prompt ends in `CSI 1 C`, so the cursor sits at offset 117; at 58 columns
/// conhost repainted the prompt as two rows, a `CSI K` row under it, and put
/// the cursor there (`CSI 19;2 H`), and likewise at 39 and 29 (117 = 3 x 39,
/// 116 = 4 x 29). The Native rewrap clamps the cursor to the
/// line's last glyph, which put it on the prompt's second row, counted one
/// content row fewer than conhost demoted, and let conhost's row-0 repaint
/// overwrite a line (`pad 16`, or `pad 13` with no `>>` under the prompt).
fn cursor_row_copy_len(row: &Row, cursor_col: u16) -> usize {
    usize::from(row.len())
        .max(usize::from(cursor_col) + 1)
        .min(row.as_slice().len())
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

    /// [`Grid::resize_no_reflow`] under an explicit [`ResizePolicy`]. The alt
    /// screen never rewraps, but a rows-grow on it still has a seam policy: an
    /// alt grid can hold ring history (a TUI that scrolled a region), and conhost
    /// repaints the alt viewport exactly as it repaints the main one.
    pub fn resize_no_reflow_with_policy(
        &mut self,
        new_rows: u16,
        new_cols: u16,
        policy: ResizePolicy,
    ) {
        self.resize_with_reflow_mode_and_policy(new_rows, new_cols, ReflowMode::Disabled, policy);
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
        // Ingress clamp (§5.8): bound the allocation a hostile resize can request.
        let new_rows = new_rows.clamp(1, MAX_GRID_ROWS);
        let new_cols = new_cols.clamp(1, MAX_GRID_COLS);
        let old_cols = self.storage.cols;
        // The alt-screen resize undo (`resize_undo`): keep the stash only if
        // nothing touched the screen since the resize that sealed it, and open
        // one for a rows-only shrink of a grid that keeps no history. Before
        // anything below moves the cursor or resets the margins, because the
        // stash compares and keeps exactly those.
        self.admit_resize_undo(new_rows, new_cols, policy);
        // This resize's row accounting starts empty: each arm of
        // `adjust_row_count` stores its own count below, and nothing an earlier
        // resize recorded may survive into this one's record (`ResizeShape`).
        self.storage.last_resize_shape = crate::ResizeShape {
            reflowed: new_cols != old_cols && reflow,
            ..crate::ResizeShape::default()
        };

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
        // Under ConPTY there is no fill at all: conhost's post-resize repaint
        // paints the freed rows `CSI K` and pulls nothing in (measured — see
        // `ResizePolicy`), so a line the fill re-seated at the top would sit
        // in the viewport AND stay painted where conhost put it: the audit's
        // duplicated wrap fragment.
        let pre_trailing_blanks = (new_cols != old_cols && reflow && !conpty)
            .then(|| self.trailing_blank_rows_below_cursor());

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
            //
            // Not under ConPTY: conhost repaints that continuation at row 0 as
            // a standalone fragment and never rejoins it with its head
            // (measured, `ResizePolicy`). Lifting it here would put the tail in
            // history while conhost paints it in the viewport too — the same
            // duplicate the fill would make — so the viewport rewrap keeps the
            // fragment where conhost keeps it, and the history rewrap ends at
            // the head, exactly as conhost's scrollback does. The fragment
            // stops being a continuation at the same moment
            // (`sever_top_row_continuation`), after the take above has read
            // the head at the full width it was filled to.
            if conpty {
                self.sever_top_row_continuation();
            } else {
                old.extend(self.take_boundary_continuation_lines());
            }
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

        let (revealed, reveal_extras) = self.adjust_row_count(new_rows, new_cols, policy);
        if revealed > 0 {
            // History handed back to the screen is live again: fence every
            // absolute-row reader of history on it (`history_reveal_gen`).
            // A ConPTY rows-grow reveals nothing (`revealed == 0`: it appends
            // blank rows at the bottom and moves the counter with them), so it
            // moves neither this fence nor any key: the history under a fence
            // taken before it is exactly the history after it.
            self.storage.history_reveal_gen = self.storage.history_reveal_gen.wrapping_add(1);
        }
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
        // this anchor exact rather than approximate. NOT in the two arms that
        // add or remove rows at the BOTTOM (the grow's blank append, the
        // shrink's trailing-blank trim): those change the retained-line total
        // and therefore slide the whole absolute space, which is why they raise
        // `history_renumber_epoch` (see `note_bottom_end_renumbered`) and why
        // the anchor degrades to approximate there — by the appended/trimmed
        // row count, and only for a reader who is already scrolled back into a
        // history too short to satisfy the grow. A WIDTH reflow renumbers
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
        //
        // On the OFFLOADED width path this `min()` is not the final word and must
        // not be read as one: `resize_offloading_scrollback` has already lifted all
        // three history layers into the job, so `scrollback_lines()` here counts
        // only what the visible rewrap just pushed back — usually nothing — and the
        // clamp lands at 0 no matter how deep the reader was. Re-attach restores
        // them from `prev_offset`, and it can tell this clamp apart from a reader who
        // chose the live bottom because THIS write is machine motion: a bare store
        // assignment, invisible to `reader_live_bottom_gen`, which only the reader's
        // own scroll primitives advance — see `scrollback_offload`'s audit-#7 guard.
        // The anchor arm below is equally invisible, and structurally so: the
        // `display_offset = 0` above runs before any of this, so the re-anchor can
        // only raise the offset, never descend to the live bottom.
        //
        // Under the ConPTY policy the two bottom arms keep the anchor EXACT:
        // they move `absolute_row_counter` with the rows they add or drop
        // (`keep_keys_across_bottom_rows`), so no key slides — and since every
        // ConPTY height drag is a blank append or a trim, that is the common
        // case there, not a corner.
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
        // A WIDTH REWRAP RENUMBERS ROWS WHOLESALE — say so for EVERY width
        // reflow, not only for the ones that spliced a row back into history.
        //
        // The three older bump sites all sit where the rewrap CROSSES the
        // live/history boundary — `prepend_ring_scrollback_lines`
        // (`scrollback_reflow.rs`), the deficit pullback
        // (`reflow_pullback.rs`) and `note_bottom_end_renumbered` — and each
        // early-returns when there is nothing to splice. So a narrowing with an
        // EMPTY scrollback (a fresh tab, or anything right after `clear`, whose
        // output still fits on screen) raised nothing at all, while still
        // rewrapping every visible logical line: the SAME lines occupy a
        // different number of rows afterwards, so absolute row N names
        // different text than it did a moment ago. That is the renumbering, and
        // `absolute_row_revision` cannot see it (only a protected-footer splice
        // moves that) while `content_gen` says only that content changed, never
        // that keys moved.
        // Measured consequence: the find overlay's highlight-all tint, which
        // fails closed on this stamp, replayed its cached `(row, col, len)` onto
        // a dash continuation row while the real hit sat untinted further down.
        //
        // Raised here rather than at the splice sites because this is the one
        // place that knows a width reflow HAPPENED. A width reflow that also
        // splices history now bumps more than once; the epoch is a monotonic
        // "did it move" counter, never an amount, so a double bump reads the
        // same as a single one. The cost of a bump is at most one extra rebuild
        // of the absolute-row-keyed caches (the viewport row cache, the
        // terminal's search index) — both of which a width change already
        // invalidates by dims — which is the trade this stamp is documented to
        // make: a spurious rebuild is harmless, a missed one is silently wrong
        // results.
        if new_cols != old_cols && reflow {
            self.storage.history_renumber_epoch =
                self.storage.history_renumber_epoch.saturating_add(1);
        }
        // Last, after this resize's own `content_gen` bump: the stash is valid
        // for the next resize only while nothing moves the generation, the
        // geometry or the cursor picture past what this one left.
        self.seal_resize_undo();
    }

    /// ConPTY width change: make the viewport's top row a line of its own
    /// instead of the continuation of the newest history line.
    ///
    /// conhost repaints such a row as a standalone fragment after every width
    /// change and never rejoins it with its head, in either direction
    /// (measured 2026-09-22, see [`ResizePolicy`]: 80→120 kept the 70-char
    /// tail at row 0, 80→60 split it 60+10, 60→80 left the split). The flag
    /// also has a cost here, not just a meaning: `take_ring_scrollback_lines`
    /// reads a history row whose successor is a continuation at its FULL
    /// width, because autowrap filled it. That holds only at the width the row
    /// was filled at. Left set across a widen, the head row is 120 wide with
    /// 80 chars in it, and the next width change reads 40 trailing blanks as
    /// content and rewraps them into blank history lines (measured on a
    /// 4-row grid holding one 150-char line: 80→120→80 grew history from 1
    /// line to 2, and a further 100→60 to 5).
    ///
    /// Addresses row 0 in CONTENT coordinates (`ring_head` + history count),
    /// which do not depend on `display_offset`. Its one caller is the ConPTY
    /// width branch of `resize_with_reflow_mode_and_policy`, right AFTER
    /// `take_scrollback_lines`, so the head is still read at the full width it
    /// was filled to; the offloaded path reaches it through its inner resize,
    /// after its own detach has taken the ring history the same way.
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
        policy: ResizePolicy,
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
            return self.adjust_row_count_rows_only(target, new_cols, policy);
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
            // Only the bottom drain changes the viewport; the front drain moves
            // history rows between tiers (`ResizeShape::pushed`).
            self.storage.last_resize_shape.pushed = row_u16(from_back);
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
                    for row in drained_rows {
                        // These are scrollback rows whose CellExtras were already
                        // extracted during normal scroll_up. Use u16::MAX as
                        // row_idx so HashMap-keyed lookups (hyperlinks, combining
                        // marks) don't misattribute visible-row extras to these
                        // scrollback rows (#7513). Ring-buffer lookups use the
                        // cell's internal index, unaffected by row_idx.
                        let extracted = Self::extract_row_extras(
                            &row,
                            &self.storage.extras,
                            u16::MAX,
                            self.styles(),
                        );
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
                    for (i, row) in drained_rows.into_iter().enumerate() {
                        let external_row = row_u16(start + i - remaining_scrollback);
                        let extracted = Self::extract_row_extras(
                            &row,
                            &self.storage.extras,
                            external_row,
                            self.styles(),
                        );
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
            self.storage.last_resize_shape.appended = row_u16(rows_to_add);
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
    /// evacuated rows' page bytes: the allocator formerly offered only bump
    /// allocation, so output refilled the ring with newly allocated bodies.
    /// Removed rows now return their unique cell slices to the SAME PageStore
    /// for reuse, keeping allocation at the retained-row high-water mark.
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
    ///
    /// The identity holds through the ring only while retention can hold the
    /// demoted rows (`max_scrollback >= demote`). The alternate screen is built
    /// with `max_scrollback = 0`, so step 4's cap evicts every demoted row in
    /// the same call and the grow finds no history to reveal. There the RESIZE
    /// UNDO (`grid::resize_undo`) keeps the evicted rows instead, and a grow
    /// with nothing drawn since hands them back (demoted rows through the
    /// reveal below, pushed rows through the bottom append), so the net-zero
    /// flap is an identity there too; once anything is drawn the undo is gone,
    /// the grow appends blanks, and the flap shifts the screen UP by the
    /// demote count. Each arm's count is recorded in [`crate::ResizeShape`] so
    /// the host can see it.
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
            if self.storage.ring_head != 0 {
                self.storage.rows.rotate_left(self.storage.ring_head);
                self.storage.ring_head = 0;
            }

            // 1) TRIM: trailing blank rows strictly below the cursor are not
            // content — archiving them would manufacture blank history that a
            // later grow reveals ABOVE real content. Drop them outright.
            let trim = shrink.min(self.trailing_blank_rows_below_cursor());
            self.storage.last_resize_shape.trimmed = row_u16(trim);
            // A trimmed row is blank, so the undo keeps only its place: the
            // grow that hands it back appends a blank row, as it always has.
            if let Some(undo) = self.storage.resize_undo.as_mut() {
                undo.entries
                    .extend(std::iter::repeat_with(|| UndoRow::Trimmed).take(trim));
            }
            if trim > 0 {
                let keep = self.storage.total_lines - trim;
                self.storage.recycle_rows(keep..self.storage.rows.len());
                self.storage.total_lines = keep;
                // Their extras keys land >= `target` after the demote shift
                // below and are swept by the caller's `retain_rows_below`.
                // RENUMBERING: dropping rows off the BOTTOM shrinks the retained
                // total while `absolute_row_counter` stays put, and
                // `oldest_absolute_row()` is `counter − visible − scrollback` —
                // so every surviving row's absolute key just shifted UP by
                // `trim`. See `note_bottom_end_renumbered`. Under ConPTY the
                // counter drops with the rows instead and no key moves: the
                // grow this trim usually undoes appended exactly such rows.
                if policy == ResizePolicy::ConPty {
                    self.keep_keys_across_bottom_rows(BottomRows::Trimmed(trim));
                } else {
                    self.note_bottom_end_renumbered();
                }
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
            self.storage.last_resize_shape.demoted = row_u16(demote);
            self.storage.last_resize_shape.pushed = row_u16(bottom_push);
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
            // `excess <= visible - target`. Removed row bodies return to the
            // arena after their content is preserved, for the next grow/scroll.
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
                } else if self.storage.resize_undo.is_some() && excess == demote + bottom_push {
                    // THE RESIZE UNDO (`resize_undo`): a grid that keeps no
                    // history had none before this shrink, so the evicted rows
                    // are exactly this call's demoted rows, then its pushed
                    // rows, in ring order. Keep them, with their extras boxes,
                    // for a grow back that nothing drew in front of. Pushed rows
                    // are kept bottom-first — the order a run of one-row
                    // shrinks would push them — so the LIFO hand-back appends
                    // them top-first.
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
                // CONPTY GROW: append the whole delta as fresh blank rows at
                // the BOTTOM and reveal nothing. conhost repaints the viewport
                // right after the resize, row-0-anchored, and paints the grown
                // rows `CSI K` (measured 24→40 and 16→30 — see
                // `ResizePolicy`): a line the Native reveal re-labelled into
                // the top of the viewport would be painted over by conhost's
                // old row 0, and since the reveal also removed it from the ring
                // history it would be gone from the buffer entirely — the
                // audit's `grow 22..37` lost for good after a 24→40 grow (and
                // all 18 history lines in the 20x60→40x120 case). Appending
                // keeps the ring history, the viewport rows and the cursor row
                // exactly where conhost keeps them; `ring_extras` are untouched
                // because no row changes tier. Unlike the Native blank append
                // below, the counter moves with the appended rows, so no
                // retained row's key slides and nothing renumbers.
                if self.storage.ring_head != 0 {
                    self.storage.rows.rotate_left(self.storage.ring_head);
                    self.storage.ring_head = 0;
                }
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
            // THE RESIZE UNDO (`resize_undo`): hand back, newest first, the rows
            // the shrink(s) this grow undoes took off. Demoted rows go back
            // ABOVE the viewport as history, for the reveal below to relabel
            // exactly as it relabels retained history; the pushed rows and the
            // trimmed blanks come back, top-first, for the bottom append.
            let restored_bottom = self.take_undo_rows(target - visible, new_cols);
            let hist = self.storage.total_lines - visible;
            let revealed = (target - visible).min(hist);
            self.storage.last_resize_shape.revealed = row_u16(revealed);
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
            // Pushed rows come back at the bottom, where the shrink took them
            // from, with their extras at their final viewport rows (injected
            // by the caller after its reveal shift, like the revealed rows').
            // Trimmed blanks come back IN PLACE among them (top-first pop
            // order), never after them: a trim stacked above a push must stay
            // above it (`take_undo_rows`).
            let mut blanks_restored = 0;
            if !restored_bottom.is_empty() {
                // Appended in order after the viewport, as the blank append
                // below does: the ring must start at its physical row 0.
                if self.storage.ring_head != 0 {
                    self.storage.rows.rotate_left(self.storage.ring_head);
                    self.storage.ring_head = 0;
                }
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
                // Added at the bottom, like the blank append below: the retained
                // total grows under a fixed counter (`note_bottom_end_renumbered`).
                self.note_bottom_end_renumbered();
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
                self.storage.last_resize_shape.appended = row_u16(blanks_restored + rows_to_add);
                // RENUMBERING: the reveal above is a pure relabel that keeps
                // absolute numbering, but these blank rows are ADDED at the
                // bottom — the retained total grows while
                // `absolute_row_counter` stays put, so `oldest_absolute_row()`
                // (`counter − visible − scrollback`) and every retained row's
                // key shift DOWN by `rows_to_add`. See
                // `note_bottom_end_renumbered`.
                self.note_bottom_end_renumbered();
            }
            return (revealed, reveal_extras);
        }
        // target == visible: nothing to reclassify.
        (0, Vec::new())
    }

    /// Record that a rows-only resize changed the retained-line total at the
    /// BOTTOM of the buffer, which RENUMBERS every retained row.
    ///
    /// `oldest_absolute_row()` is `absolute_row_counter − visible_rows −
    /// scrollback_lines()`, i.e. `counter − retained_total`, and a rows-only
    /// resize deliberately leaves `absolute_row_counter` alone. Moving rows
    /// ACROSS the live/history boundary (the grow-side reveal, the shrink-side
    /// top-demote) keeps `retained_total` fixed and is therefore the pure
    /// relabel the resize docs describe — every absolute key survives it. Two
    /// arms are not relabels: the shrink's trailing-blank TRIM removes rows from
    /// the bottom and the grow's blank APPEND adds them there. Both change
    /// `retained_total`, so `oldest_absolute_row()` slides and EVERY retained
    /// row's `oldest + i` key slides with it, while `content_gen` (a bulk
    /// mutation), `absolute_row_revision` (no splice) and `base_y()` arithmetic
    /// look exactly like an ordinary resize.
    ///
    /// That is the definition of the epoch this bumps: a wholesale history
    /// renumbering invisible to the `(content_gen, absolute_row_revision,
    /// geometry)` key set. Without it, absolute-row-keyed caches carried
    /// pre-shift keys forward — the GUI search index re-fed only the rows
    /// at/after the OLD visible base and dropped the top band of the screen out
    /// of the search corpus entirely, so find reported "no matches" for text
    /// plainly on the glass after `clear` + a taller window. Same reasoning and
    /// the same fix as the Kitty unscroll (`scroll_unscroll.rs`) and the
    /// scrollback rewrap (`scrollback_reflow.rs`): a spurious rebuild is
    /// harmless, a missed one is silently wrong results.
    fn note_bottom_end_renumbered(&mut self) {
        self.storage.history_renumber_epoch = self.storage.history_renumber_epoch.saturating_add(1);
    }

    /// The ConPTY counterpart of [`Self::note_bottom_end_renumbered`]: move
    /// `absolute_row_counter` WITH the rows a rows-only resize appended at or
    /// trimmed from the bottom, so `oldest_absolute_row()` (`counter −
    /// retained_total`) stays put and every retained row keeps its key. Nothing
    /// is renumbered, so the epoch does not move and absolute-row-keyed caches
    /// refresh instead of rebuilding.
    ///
    /// Appended rows take fresh keys above every key in use. Trimmed rows were
    /// blank rows below the cursor, and their keys go to the rows that next
    /// appear there, as conhost's own buffer does. Those keys named live rows
    /// only (a ConPTY rows-only resize never reveals history), and the
    /// history-row memo in `visible_row_view` keys on `content_gen`, which
    /// every resize bumps, so a reused key cannot hit a stale memo entry.
    ///
    /// A reader aim held across an off-thread reflow (ruling 238) reads the
    /// counter's rise as the lines that scrolled into history since it was
    /// taken, so its `at_counter` moves by the same amount: no line scrolled.
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
    /// When shrinking columns causes wrapping that produces more rows than
    /// `target_rows`, excess rows from the top are pushed to scrollback (lazy
    /// buffer) before truncation, preserving cursor content. (#7410)
    ///
    /// Drops old grid data before allocating padding rows so that peak memory
    /// during resize is reduced — the old page store is freed before new
    /// empty-row pages are allocated (#4074).
    ///
    /// Under [`ResizePolicy::ConPty`] the count pushed is the one conhost
    /// demotes (see [`Self::conpty_reflow_demote`]), not the Native minimum
    /// that only brings the cursor on screen.
    fn finalize_reflow(
        &mut self,
        target_rows: u16,
        mut result: ReflowResult,
        new_cols: u16,
        policy: ResizePolicy,
    ) {
        let target_rows = usize::from(target_rows);

        // Push excess top rows to scrollback instead of silently discarding
        // them (#7410). Native pushes only when the cursor overflows the
        // visible area, and only the minimum that brings it back into view;
        // ConPTY pushes what conhost demotes.
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
            // for extract_row_extras while iterating (#7448).
            let drained_rows: Vec<Row> = result.rows.drain(..push_count).collect();
            for (i, row) in drained_rows.into_iter().enumerate() {
                let row_idx = u16::try_from(i).unwrap_or(u16::MAX);
                let extracted =
                    Self::extract_row_extras(&row, &result.extras, row_idx, self.styles());
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

    /// How many top rows of a rewrapped viewport conhost demotes to its
    /// scrollback when a ConPTY width change leaves more rows than fit: the
    /// rows that carry content (through the cursor's row and the last
    /// non-blank row, so blank rows under the cursor are simply dropped)
    /// minus the target height, at most the rows above the cursor — the
    /// rows-only shrink's trim + top-demote, applied to the rewrapped rows.
    ///
    /// The Native count is only what brings the cursor on screen, and that
    /// loses a line under conhost whenever a non-blank row sits below the
    /// cursor. Measured 2026-09-27 (pwsh 7.6, aterm 0.95.0, `aterm ctl
    /// cast`): the pad fill left PSReadLine's `>>` on row 23 of a 24x80 tab
    /// with the cursor on row 22; after a resize to 20x60 conhost repainted
    /// `pad 16` at row 0 and `>>` on the bottom row (`CSI 19;58 H`), i.e. it
    /// demoted all four rows, `pad 12..=15`. The Native count pushed three,
    /// left `pad 15` at row 0 for conhost to paint `pad 16` over, and cut
    /// `>>`: `pad 15` was gone from history and screen alike. 20x50 lost
    /// `pad 17` the same way.
    ///
    /// The count is only as good as `result.cursor_row`: it matches conhost's
    /// because the ConPTY rewrap keeps the cursor's row through the cursor's
    /// cell (`cursor_row_copy_len`). With the cursor clamped to the prompt's
    /// last glyph, 20x58 still counted one row fewer than conhost demoted.
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
        policy: ResizePolicy,
    ) {
        let mut new_pages = PageStore::new();
        let visible_count = usize::from(self.storage.visible_rows);
        let mut new_rows: Vec<Row> = Vec::with_capacity(visible_count);
        let mut cursor = (cursor_row, cursor_col);
        let mut merge_buf: Vec<super::Cell> = Vec::with_capacity(self.storage.cols as usize);
        let mut merge_coords: Vec<CellCoord> = Vec::new();
        let mut new_extras = CellExtras::new();
        let keep_cursor_cell = policy == ResizePolicy::ConPty;

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
                    keep_cursor_cell,
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
            } else if copy_len == 0 {
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
                let cells = &row.as_slice()[..copy_len];
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
            policy,
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
        keep_cursor_cell: bool,
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

        // ConPTY: a cursor on the line's LAST row keeps every cell through its
        // own (`cursor_row_copy_len`). On an earlier row the autowrap pad above
        // already reaches past the cursor, so only the last row can need it.
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

    // =========================================================================
    // ResizePolicy::ConPty — the seam accounting conhost's repaint requires
    // (measured 2026-09-22, see the enum docs)
    // =========================================================================

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

    /// A ConPTY rows-grow appends the whole delta as blank rows at the bottom:
    /// the ring history is untouched, the old viewport rows stay at rows 0..,
    /// the cursor row is unchanged, no row shift is published for the
    /// selection — and the counter moved with the appended rows, so every
    /// retained row kept its absolute key and nothing was renumbered.
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
    /// alike — is the same at each step, with no renumbering in between. (The
    /// Native pair for a grow that finds no history to reveal renumbers on
    /// each step instead — `tests/reflow/rows_only_identity.rs` pins that.)
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
    /// out for an off-thread rewrap (ruling 238) lands exactly where they
    /// asked, though a ConPTY grow moved the counter meanwhile: the aim counts
    /// the counter's rise as lines that scrolled into history, and a grow
    /// scrolls none. Without the aim moving with the counter the re-attach
    /// lands them 6 rows too far up.
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

    /// The contrast that makes the test above two-sided: the Native grow on
    /// the same grid REVEALS the four newest history lines at the top — the
    /// move conhost's repaint then paints over.
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
    /// rewrap nor fills the viewport from history: the tail stays at row 0 as
    /// the standalone fragment conhost repaints there, the head stays the one
    /// history line (conhost's scrollback keeps it unmerged too), and the rows
    /// below are untouched.
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
    /// head re-enters every later rewrap at the 10 chars it holds, not at the
    /// 20-col width its ring row was rebuilt to. Fails with the sever removed:
    /// the still-flagged fragment makes the next take read the head's 10
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
    /// back into the viewport (the fixwave5 bottom-anchoring) — correct when
    /// the grid is the frame, and exactly the move conhost paints over.
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

    /// Rows-shrink is policy-independent: conhost's measured shrink repaint
    /// (trailing blanks dropped, else a top-demote) is the Native shape.
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
    /// `cursor_row`, ring retention `max_scrollback` — 0 is the alternate
    /// screen's shape (`Grid::with_scrollback(rows, cols, 0)` in the DEC 47/1049
    /// handlers).
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

    /// `ResizeShape` names each arm a resize ran, with its count: TRIM below the
    /// cursor, TOP-DEMOTE, the BOTTOM-PUSH corner, the grow's REVEAL and blank
    /// APPEND, and the width rewrap. The retention-0 rows are the alternate
    /// screen's flap: the shrink's demote is evicted from the ring but kept by
    /// the resize undo, so the grow hands it back (`restored_top`) and the flap
    /// is an identity; with a write between the halves the undo is gone, the
    /// grow can only append, and the screen stays shifted up by the demote count.
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

        // TRIM: two content rows, cursor on the second, four blank rows below.
        let mut grid = labelled_screen(2, 1, 100);
        grid.resize(6, 10);
        assert_eq!(grid.take_last_resize_shape(), shape(0, 0, 0, 0, 4));
        grid.resize(4, 10);
        assert_eq!(grid.last_resize_shape(), shape(2, 0, 0, 0, 0), "peek");
        assert_eq!(grid.take_last_resize_shape(), shape(2, 0, 0, 0, 0));
        assert_eq!(grid.last_resize_shape(), ResizeShape::default(), "drained");
        assert_eq!(grid.take_last_resize_row_shift(), 0, "a trim moves nothing");
        assert_eq!(grid.row(0).unwrap().to_string(), "R0");

        // TRIM, then DEMOTE for the rest: one blank row below the cursor.
        let mut grid = labelled_screen(6, 5, 100);
        grid.erase_line();
        grid.set_cursor(4, 0);
        grid.resize(3, 10);
        assert_eq!(grid.take_last_resize_shape(), shape(1, 2, 0, 0, 0));
        assert_eq!(grid.row(0).unwrap().to_string(), "R2");
        assert_eq!(grid.cursor_row(), 2);

        // DEMOTE on a retaining grid, and the grow REVEALS it back: identity.
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

        // The same flap with NO retention: the demote is evicted from the ring
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

        // BOTTOM-PUSH corner: a full screen with the cursor on row 0.
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

    /// PSReadLine's shape from the 2026-09-27 capture, in small: `L0..=L5` in
    /// history, `L6..=L9` on rows 0..4, a prompt `>` on row 4 with the cursor
    /// after it, and a non-blank `>>` on row 5 BELOW the cursor.
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

    /// pwsh's prompt from the 2026-09-27 capture, in small: `L0..=L5` in
    /// history, `L6..=L9` on rows 0..=3, and a 15-cell prompt that autowrapped
    /// at 10 columns (`P` x10 on row 4, `Q` x5 on row 5) with the cursor one
    /// column past its last glyph, as pwsh's `CSI 1 C` leaves it: offset 16.
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
    /// (`cursor_row_copy_len`; measured with pwsh's 116-column prompt at 58),
    /// and that row counts toward the demote: 8 content rows into 6 demotes
    /// two. Native clamps the cursor to the last glyph, counts 7 rows and
    /// pushes one — the row conhost's row-0 repaint then overwrites.
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

    /// The rule holds on a row with no text at all. Measured 2026-09-27
    /// (pwsh 7.6 in a debug aterm 0.95.0 of this tree, `aterm ctl cast`):
    /// `cls`, the cursor parked at column 70 of row 5 by
    /// `[Console]::SetCursorPosition` during a `Start-Sleep`; conhost's repaint
    /// put it at `CSI 7;11 H` after 80→60, `CSI 7;31 H` after 60→40 and
    /// `CSI 6;71 H` after 40→80. Native keeps it on its row, clamped.
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
