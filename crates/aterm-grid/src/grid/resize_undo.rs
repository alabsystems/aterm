// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! THE RESIZE UNDO: a rows-only shrink on a grid that keeps no history, then a
//! grow back with nothing drawn in between, is an exact identity.
//!
//! WHY (measured 2026-09-28 against a live Claude Code tab). The window
//! re-gridded 121x52→51→52 within 0.8 ms, about once a minute. The alternate
//! screen keeps no history (`max_scrollback = 0`), so the shrink's TOP-DEMOTE was
//! evicted in the same call and the grow could only append a blank row: every
//! row sat one higher than the app painted it. Claude Code repaints only when
//! its SIGWINCH handler reads a CHANGED size; when both halves landed before the
//! handler ran it read 52 == 52, skipped the repaint, and diffed onto the
//! shifted rows for good. Whatever re-grids a window, the app's picture must
//! survive a flap it never saw, so the engine undoes it.
//!
//! HOW. A Native rows-only shrink of a grid with no retention and no tiered
//! store (the alt screen's shape) keeps what it takes off the screen in a
//! stash, one entry per row in unit-shrink order: TRIMMED blank rows (nothing
//! to keep: a blank append rebuilds them), then the DEMOTED top rows, then the
//! PUSHED bottom rows, each with its extras box. A later shrink stacks onto the
//! stash; a grow pops it LIFO, re-inserting demoted rows above the viewport
//! (the ordinary reveal then relabels them, moving the cursor, the saved
//! cursor, the extras and the selection back down) and appending pushed rows
//! where it would append blanks. When the grow lands back on the height the
//! first shrink started from, the cursor picture that shrink found (cursor,
//! pending wrap, the grid's saved cursor, the scroll region and the horizontal
//! margins, which every resize resets) is put back too, and the stash goes.
//!
//! WHEN IT IS VALID. Only while nothing could have seen or changed the screen
//! the shrink left: the grid's `content_gen` is where the last resize sealed it,
//! the width, height and cursor picture are what that resize left, the grid
//! still keeps no history and the policy is Native. Anything else DROPS the
//! stash, and the resize runs exactly as it always has (the historical shift,
//! which the app's own repaint heals). `Terminal::process_at` drops it on any
//! input at all, so no mutation path that marks damage without bumping
//! `content_gen` can slip between the halves; an alt-screen switch and a reset
//! drop it too. An app that drew at the smaller size keeps today's behaviour:
//! its SIGWINCH repaint at the changed size is what heals it.
//!
//! Why ANY input, not only a `content_gen` move: not every mutation path bumps
//! `content_gen` (VS15/VS16 presentation fixes and OSC 1337 are two that did
//! not), so a narrower rule would hand back rows over a screen that changed.
//! The cost is that output which draws nothing (a mode set, a query) between
//! the halves still leaves the historical shift; an app whose handler ran
//! there saw the changed size and repaints, but an unrelated spinner frame
//! does not. The undo makes the incident's flap (nothing between the halves)
//! an identity; it does not close the window entirely, and the host must still
//! not flap.
//!
//! Session-only: never checkpointed, never journalled beyond the counts in
//! [`crate::ResizeShape`]. A checkpoint or live upgrade between the halves
//! restores a grid with no undo, so that flap keeps the historical shift.
//!
//! The selection lives on the host, not here: `Terminal` keeps the one the
//! first shrink found and puts it back on [`crate::ResizeShape::undone`],
//! keyed by [`Grid::resize_undo_serial`].

use super::scroll_convert::ScrolledRowExtras;
use super::{Cursor, Grid, HorizontalMargins, ScrollRegion};
use crate::grid::reflow::ResizePolicy;
use crate::row::RowSnapshot;

/// One row a shrink took off the screen, in unit-shrink order.
#[derive(Debug)]
pub(crate) enum UndoRow {
    /// A trailing blank row below the cursor, dropped off the bottom.
    Trimmed,
    /// A top row demoted into (and evicted from) the absent history.
    Demoted(RowSnapshot, Option<Box<ScrolledRowExtras>>),
    /// A bottom row pushed off the screen (the cursor sat too high to demote).
    Pushed(RowSnapshot, Option<Box<ScrolledRowExtras>>),
}

/// One row a grow puts back at the bottom: a pushed row with its extras
/// (`Some`), or a trimmed blank (`None`).
pub(crate) type BottomRow = Option<(RowSnapshot, Option<Box<ScrolledRowExtras>>)>;

/// The cursor-owned state a resize resets or moves, compared whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CursorPicture {
    cursor: Cursor,
    pending_wrap: bool,
    saved_cursor: Cursor,
    saved_valid: bool,
    saved_pending_wrap: bool,
    scroll_region: ScrollRegion,
    margins: HorizontalMargins,
    has_margins: bool,
}

/// The stash (see the module doc).
#[derive(Debug)]
pub(crate) struct ResizeUndo {
    /// LIFO: the newest shrink's rows are at the end.
    pub(crate) entries: Vec<UndoRow>,
    /// The width every entry was taken at.
    cols: u16,
    /// The height the FIRST shrink started from, and the cursor picture it
    /// found there.
    rows_before: u16,
    pre: CursorPicture,
    /// What the last resize that touched the stash left: height, content
    /// generation, cursor picture.
    rows_after: u16,
    seal_gen: u64,
    after: CursorPicture,
    /// Which stash this is, process-unique ([`Grid::resize_undo_serial`]).
    serial: u64,
}

/// The next stash's serial. Process-wide so two grids (or a stash that was
/// dropped and a fresh one opened in its place) never share one.
static NEXT_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Grid {
    fn cursor_picture(&self) -> CursorPicture {
        let s = &self.storage;
        CursorPicture {
            cursor: s.cursor,
            pending_wrap: s.pending_wrap,
            saved_cursor: s.saved_cursor.cursor,
            saved_valid: s.saved_cursor.valid,
            saved_pending_wrap: s.saved_cursor.pending_wrap,
            scroll_region: s.cursor_state.scroll_region,
            margins: s.cursor_state.horizontal_margins,
            has_margins: s.has_horizontal_margins,
        }
    }

    fn set_cursor_picture(&mut self, p: CursorPicture) {
        let s = &mut self.storage;
        s.cursor = p.cursor;
        s.pending_wrap = p.pending_wrap;
        s.saved_cursor.cursor = p.saved_cursor;
        s.saved_cursor.valid = p.saved_valid;
        s.saved_cursor.pending_wrap = p.saved_pending_wrap;
        s.cursor_state.scroll_region = p.scroll_region;
        s.cursor_state.horizontal_margins = p.margins;
        s.has_horizontal_margins = p.has_margins;
    }

    /// The grid shape a stash is kept for: no retention, no tiered store, no
    /// history, under the Native policy.
    fn keeps_resize_undo(&self, policy: ResizePolicy) -> bool {
        policy == ResizePolicy::Native
            && self.storage.max_scrollback == 0
            && !self.storage.stages_evicted_rows()
            && self.storage.total_lines == usize::from(self.storage.visible_rows)
    }

    /// Run first by every resize, before anything moves: keep the stash only if
    /// nothing touched the screen since the resize that sealed it, and open a
    /// fresh one for a rows-only shrink on a grid of the kept shape.
    pub(super) fn admit_resize_undo(&mut self, new_rows: u16, new_cols: u16, policy: ResizePolicy) {
        let shape = self.keeps_resize_undo(policy) && new_cols == self.storage.cols;
        let picture = self.cursor_picture();
        let valid = shape
            && self.storage.resize_undo.as_ref().is_some_and(|u| {
                u.cols == new_cols
                    && u.rows_after == self.storage.visible_rows
                    && u.seal_gen == self.storage.content_gen
                    && u.after == picture
            });
        if !valid {
            self.storage.resize_undo = None;
        }
        if shape && self.storage.resize_undo.is_none() && new_rows < self.storage.visible_rows {
            self.storage.resize_undo = Some(Box::new(ResizeUndo {
                entries: Vec::new(),
                cols: new_cols,
                rows_before: self.storage.visible_rows,
                pre: picture,
                rows_after: self.storage.visible_rows,
                seal_gen: self.storage.content_gen,
                after: picture,
                serial: NEXT_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            }));
        }
    }

    /// Run last by every resize: seal the stash at what this resize left, or,
    /// once a grow has handed every entry back on the first shrink's height,
    /// put that shrink's cursor picture back and drop it.
    pub(super) fn seal_resize_undo(&mut self) {
        let Some(undo) = self.storage.resize_undo.as_ref() else {
            return;
        };
        if undo.entries.is_empty() {
            let back = (undo.rows_before == self.storage.visible_rows).then_some(undo.pre);
            self.storage.resize_undo = None;
            if let Some(pre) = back {
                self.set_cursor_picture(pre);
                self.storage.last_resize_shape.undone = true;
            }
            return;
        }
        let picture = self.cursor_picture();
        let (rows, generation) = (self.storage.visible_rows, self.storage.content_gen);
        if let Some(undo) = self.storage.resize_undo.as_mut() {
            undo.rows_after = rows;
            undo.seal_gen = generation;
            undo.after = picture;
        }
    }

    /// The Native rows-grow's half: pop up to `grow` entries (newest first).
    /// Demoted rows are re-inserted ABOVE the viewport as history, oldest on
    /// top, with their extras boxes aligned in `ring_extras`, so the reveal that
    /// follows relabels them into the viewport and moves every other row, the
    /// cursor and the selection back down by their count. Everything taken off
    /// the BOTTOM — pushed rows (`Some`) and trimmed blanks (`None`, a fresh
    /// blank row) — is returned in pop order, which is top-first, for the
    /// caller to append in exactly that order. A trimmed entry popped before a
    /// pushed one sat ABOVE it: a push at 8→7 then a trim at 7→6 left
    /// `[L5, blank, X]` on the bottom, and a single grow 6→8 that appended the
    /// pushed rows before the blanks (the first cut of this undo) gave back
    /// `[L5, X, blank]` while the ledger read `displaced=0`.
    ///
    /// A stash is only admitted on a grid with no history
    /// (`keeps_resize_undo`), so every row inserted here is the whole history.
    pub(super) fn take_undo_rows(&mut self, grow: usize, cols: u16) -> Vec<BottomRow> {
        let Some(undo) = self.storage.resize_undo.as_mut() else {
            return Vec::new();
        };
        let mut demoted = Vec::new();
        let mut bottom = Vec::new();
        for _ in 0..grow {
            match undo.entries.pop() {
                Some(UndoRow::Demoted(row, extras)) => demoted.push((row, extras)),
                Some(UndoRow::Pushed(row, extras)) => bottom.push(Some((row, extras))),
                Some(UndoRow::Trimmed) => bottom.push(None),
                None => break,
            }
        }
        if !demoted.is_empty() {
            debug_assert_eq!(
                self.storage.total_lines,
                usize::from(self.storage.visible_rows),
                "a resize undo is only kept on a grid with no history"
            );
            let storage = &mut self.storage;
            if storage.ring_head != 0 {
                storage.rows.rotate_left(storage.ring_head);
                storage.ring_head = 0;
            }
            // Popped newest first; the oldest demoted row is the top one.
            demoted.reverse();
            let pages = &mut storage.pages;
            let rows: Vec<_> = demoted
                .iter()
                // SAFETY: the rows are stored in the same `GridStorage` that owns
                // `pages`, and rows drop before the backing pages.
                .map(|(row, _)| unsafe { crate::Row::from_snapshot(row, cols, pages) })
                .collect();
            let n = rows.len();
            drop(storage.rows.splice(0..0, rows));
            storage.total_lines += n;
            // No history means no history extras: the deque is realigned to
            // exactly the rows just inserted, oldest first.
            storage.ring_extras.clear();
            storage
                .ring_extras
                .extend(demoted.into_iter().map(|(_, extras)| extras));
            storage.last_resize_shape.restored_top = crate::row_u16(n);
        }
        bottom
    }

    /// Forget the stash: the next grow appends blanks, as it always did. Called
    /// on any output ([`crate::Grid`] owners: `Terminal::process_at`), on an
    /// alternate-screen switch and on a reset.
    #[inline]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "ResizeRowReuse",
            action = "Output",
            project = "grid::tests::resize_row_reuse::resize_row_reuse_conforms"
        )
    )]
    pub fn drop_resize_undo(&mut self) {
        if self.storage.resize_undo.is_some() {
            self.storage.resize_undo = None;
        }
    }

    /// The open resize undo's serial (`None`: no undo). A host that keeps state
    /// of its own across a flap (the Terminal's selection) keys it by this: the
    /// same serial after a resize means the same stash, still unbroken, and
    /// [`crate::ResizeShape::undone`] says when it has been handed back whole.
    #[must_use]
    pub fn resize_undo_serial(&self) -> Option<u64> {
        self.storage.resize_undo.as_ref().map(|u| u.serial)
    }

    /// How many rows with content a grow could hand back right now: the demoted
    /// and pushed rows the resize undo holds (0: no undo). Trimmed blank rows
    /// are not counted; a grow rebuilds them as the blanks it appends.
    #[must_use]
    pub fn resize_undo_rows(&self) -> usize {
        self.storage.resize_undo.as_ref().map_or(0, |u| {
            u.entries
                .iter()
                .filter(|e| !matches!(e, UndoRow::Trimmed))
                .count()
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::grid::Grid;

    /// `rows` rows labelled `R0`, `R1`, … (every row non-blank, the full tail an
    /// alt-screen TUI paints), cursor parked on `cursor_row`, ring retention
    /// `max_scrollback` (0 is the alternate screen's shape).
    fn labelled(rows: u16, cols: u16, cursor_row: u16, max_scrollback: usize) -> Grid {
        let mut grid = Grid::with_scrollback(rows, cols, max_scrollback);
        for r in 0..rows {
            grid.set_cursor(r, 0);
            for c in format!("R{r}").chars() {
                grid.write_char(c);
            }
        }
        grid.set_cursor(cursor_row, 2);
        grid
    }

    fn screen(grid: &Grid) -> Vec<String> {
        (0..grid.rows())
            .map(|r| grid.row(r).map(ToString::to_string).unwrap_or_default())
            .collect()
    }

    /// Everything a flap may not change: every row's text, the cursor, the
    /// pending wrap, the scroll region and the horizontal margins.
    fn picture(grid: &Grid) -> (Vec<String>, u16, u16, bool, crate::ScrollRegion) {
        (
            screen(grid),
            grid.cursor_row(),
            grid.cursor_col(),
            grid.pending_wrap(),
            grid.scroll_region(),
        )
    }

    /// The incident's shape: 121x52 on a grid that keeps no history, every row
    /// painted, the cursor on the bottom row, a scroll region set. 52→51→52 with
    /// nothing between the halves hands the demoted top row back.
    #[test]
    fn a_quiet_52_51_52_flap_is_an_identity() {
        let mut grid = labelled(52, 121, 51, 0);
        grid.set_scroll_region(3, 40);
        grid.set_cursor(51, 7);
        let before = picture(&grid);

        grid.resize_no_reflow(51, 121);
        let shrink = grid.take_last_resize_shape();
        assert_eq!((shrink.demoted, shrink.stashed), (1, 1));
        assert_eq!(grid.row(0).unwrap().to_string(), "R1", "the shrink demotes");
        assert_eq!(grid.resize_undo_rows(), 1);

        grid.resize_no_reflow(52, 121);
        let grow = grid.take_last_resize_shape();
        assert_eq!(
            (grow.revealed, grow.restored_top, grow.appended),
            (1, 1, 0),
            "the grow hands the row back instead of appending a blank"
        );
        assert_eq!(picture(&grid), before, "an exact identity");
        assert_eq!(grid.resize_undo_rows(), 0, "the undo is spent");
        assert_eq!(grid.scrollback_lines(), 0);
        grid.assert_invariants();
    }

    /// Stacked shrinks stack onto one undo, and a partial grow hands back only
    /// the newest rows, one per grown row, in LIFO order.
    #[test]
    fn stacked_shrinks_and_partial_grows_unwind_in_order() {
        let mut grid = labelled(6, 10, 4, 0);
        let before = picture(&grid);
        grid.resize_no_reflow(5, 10);
        grid.resize_no_reflow(4, 10);
        assert_eq!(grid.row(0).unwrap().to_string(), "R2");
        assert_eq!(grid.resize_undo_rows(), 2);
        grid.resize_no_reflow(6, 10);
        assert_eq!(picture(&grid), before, "6→5→4→6 is an identity");

        let mut grid = labelled(6, 10, 4, 0);
        grid.resize_no_reflow(4, 10);
        grid.resize_no_reflow(5, 10);
        let shape = grid.take_last_resize_shape();
        assert_eq!((shape.restored_top, shape.appended), (1, 0));
        assert_eq!(grid.row(0).unwrap().to_string(), "R1", "the newest back");
        assert_eq!(grid.cursor_row(), 3);
        assert_eq!(grid.resize_undo_rows(), 1);
        grid.resize_no_reflow(6, 10);
        assert_eq!(picture(&grid), before, "and then the rest");

        // A grow past the first shrink's height hands everything back and
        // appends blanks for the rest.
        let mut grid = labelled(6, 10, 4, 0);
        grid.resize_no_reflow(5, 10);
        grid.resize_no_reflow(8, 10);
        let shape = grid.take_last_resize_shape();
        assert_eq!((shape.restored_top, shape.appended), (1, 2));
        assert_eq!(screen(&grid)[..6], before.0[..]);
        assert_eq!(grid.resize_undo_rows(), 0);
        grid.assert_invariants();
    }

    /// The bottom-push corner (the cursor too high to demote) and a trim both
    /// unwind too: pushed rows come back at the bottom, trimmed rows as blanks.
    #[test]
    fn pushed_and_trimmed_rows_come_back_where_they_were() {
        for cursor in [0, 1] {
            let mut grid = labelled(6, 10, cursor, 0);
            let before = picture(&grid);
            grid.resize_no_reflow(4, 10);
            let shrink = grid.take_last_resize_shape();
            assert_eq!(
                (shrink.demoted + shrink.pushed, shrink.stashed),
                (2, 2),
                "cursor {cursor}"
            );
            grid.resize_no_reflow(6, 10);
            let grow = grid.take_last_resize_shape();
            assert_eq!(
                (grow.restored_bottom, grow.appended),
                (2 - shrink.demoted, 0)
            );
            assert_eq!(picture(&grid), before, "cursor {cursor}");
            grid.assert_invariants();
        }

        // TRIM then DEMOTE: one blank row below the cursor.
        let mut grid = labelled(6, 10, 5, 0);
        grid.erase_line();
        grid.set_cursor(4, 1);
        let before = picture(&grid);
        grid.resize_no_reflow(3, 10);
        assert_eq!(
            grid.resize_undo_rows(),
            2,
            "two demoted; the trim is a blank"
        );
        grid.resize_no_reflow(6, 10);
        assert_eq!(picture(&grid), before);
    }

    /// A PUSH then a TRIM, handed back by ONE grow: the trimmed blank sat above
    /// the pushed row and must come back above it. (Adversarial review of the
    /// first cut: 8→7 pushed `X`, 7→6 trimmed the blank now at the bottom, and
    /// the grow 6→8 appended `X` before the blank, `[L5, X, blank]`, while the
    /// ledger read `displaced=0 render=ok`.) Every unwind order agrees.
    #[test]
    fn a_trim_stacked_on_a_push_comes_back_above_it() {
        fn painted() -> Grid {
            let mut grid = labelled(8, 20, 0, 0);
            grid.set_cursor(6, 0);
            grid.erase_line();
            grid.set_cursor(7, 0);
            grid.erase_line();
            grid.write_char('X');
            grid.set_cursor(0, 0);
            grid
        }
        let steps: [&[u16]; 4] = [&[7, 6, 8], &[7, 6, 7, 8], &[6, 8], &[7, 6, 5, 8]];
        for heights in steps {
            let mut grid = painted();
            let before = picture(&grid);
            assert_eq!(before.0[5..], ["R5", "", "X"]);
            for &h in heights {
                grid.resize_no_reflow(h, 20);
            }
            assert_eq!(picture(&grid), before, "{heights:?}");
            assert_eq!(grid.resize_undo_rows(), 0, "{heights:?}");
            grid.assert_invariants();
        }

        // The grow's counts: one pushed row restored, one blank rebuilt.
        let mut grid = painted();
        grid.resize_no_reflow(7, 20);
        grid.resize_no_reflow(6, 20);
        grid.resize_no_reflow(8, 20);
        let grow = grid.take_last_resize_shape();
        assert_eq!((grow.restored_bottom, grow.appended), (1, 1));
    }

    /// Anything between the halves that could have shown the app the smaller
    /// screen, or changed it, drops the undo: the grow appends as it always
    /// did and the screen stays shifted (the app's own repaint heals it).
    #[test]
    fn a_write_a_cursor_move_or_a_width_change_drops_the_undo() {
        type Between = fn(&mut Grid);
        let cases: [(&str, Between); 4] = [
            ("write", |g| g.write_char('x')),
            ("cursor move", |g| g.set_cursor(0, 0)),
            ("scroll region", |g| g.set_scroll_region(0, 2)),
            ("drop", Grid::drop_resize_undo),
        ];
        for (what, between) in cases {
            let mut grid = labelled(6, 10, 4, 0);
            grid.resize_no_reflow(5, 10);
            between(&mut grid);
            grid.resize_no_reflow(6, 10);
            let grow = grid.take_last_resize_shape();
            assert_eq!((grow.restored_top, grow.appended), (0, 1), "{what}");
            assert_eq!(grid.row(0).unwrap().to_string(), "R1", "{what}: shifted");
            assert!(grid.row(5).unwrap().is_empty(), "{what}");
        }

        // A width change between the halves.
        let mut grid = labelled(6, 10, 4, 0);
        grid.resize_no_reflow(5, 10);
        grid.resize_no_reflow(5, 12);
        assert_eq!(grid.resize_undo_rows(), 0);
        grid.resize_no_reflow(6, 12);
        assert_eq!(grid.row(0).unwrap().to_string(), "R1");

        // The ConPTY policy never keeps one.
        let mut grid = labelled(6, 10, 4, 0);
        grid.resize_with_policy(5, 10, crate::grid::reflow::ResizePolicy::ConPty);
        assert_eq!(grid.resize_undo_rows(), 0);
    }

    /// A grid that keeps history never needs one: its flap is already an
    /// identity through the ring (and a tiered store stages what it evicts).
    #[test]
    fn a_retaining_grid_keeps_no_undo() {
        let mut grid = labelled(6, 10, 4, 100);
        let before = picture(&grid);
        grid.resize(5, 10);
        assert_eq!(grid.resize_undo_rows(), 0);
        assert_eq!(grid.take_last_resize_shape().stashed, 0);
        grid.resize(6, 10);
        let grow = grid.take_last_resize_shape();
        assert_eq!((grow.revealed, grow.restored_top), (1, 0));
        assert_eq!(picture(&grid).0, before.0);
    }
}
